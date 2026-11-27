//! streaming witnesses, split from the carrier by subject.
//!
//! Moved unchanged: the ratchet bounds a FILE, and a witness that was edited
//! on its way out of one would be a different witness.

use super::*;

#[test]
fn a_prompt_turn_renders_reasoning_text_usage_and_a_terminal() {
    // Deliverable 1 end to end, over the wire, through the same decoder the
    // remote transport would use.
    let mut client = client();
    let Ok(_) = client.create_session() else {
        panic!("create");
    };
    let Ok(()) = client.submit_prompt("hello") else {
        panic!("prompt");
    };
    let Ok(_) = client.pump() else { panic!("pump") };

    let Some(view) = client.view() else {
        panic!("the selected session has a view")
    };
    let kinds: Vec<SliceKind> = view.slices.iter().map(|s| s.kind).collect();
    assert!(kinds.contains(&SliceKind::Reasoning), "reasoning rendered");
    assert!(kinds.contains(&SliceKind::Text), "text rendered");
    assert!(
        view.slices.iter().any(|s| s.text.contains("echo hello")),
        "the reply text accumulated: {:?}",
        view.slices
    );
    assert_eq!(view.usage.input, Some(12));
    assert_eq!(view.usage.output, Some(7));
    assert!(
        matches!(
            view.execution,
            ExecutionState::Settled {
                outcome: Outcome::Succeeded,
                ..
            }
        ),
        "the execution settled: {:?}",
        view.execution
    );
    assert_eq!(
        view.idle, None,
        "terminal hydration does not invent quiescence"
    );
}

#[test]
fn reconciling_converges_to_durable_truth() {
    // THE invariant. After a pump the client has reconciled from the real
    // re-list, so its view must equal the one built from durable truth alone.
    let mut client = client();
    let Ok(id) = client.create_session() else {
        panic!("create");
    };
    let Ok(()) = client.submit_prompt("hello") else {
        panic!("prompt");
    };
    let Ok(_) = client.pump() else { panic!("pump") };
    // Reconcile explicitly so both sides are the durable projection.
    let Ok(()) = client.reconcile(&id) else {
        panic!("reconcile")
    };

    let lived = match client.view() {
        Some(view) => view.clone(),
        None => panic!("a view exists"),
    };
    let reference = durable_only(client.peer_mut(), &id);
    assert_eq!(
        lived.slices, reference.slices,
        "the reconciled transcript equals durable truth exactly"
    );
}

#[test]
fn a_closed_stream_reconciles_without_duplicating_the_transcript() {
    // The reconnect law. The dangerous failure is not losing content — it is
    // re-rendering content the user already saw.
    let mut client = client();
    let Ok(_) = client.create_session() else {
        panic!("create");
    };
    let Ok(()) = client.submit_prompt("hello") else {
        panic!("prompt");
    };
    let Ok(_) = client.pump() else { panic!("pump") };
    let Ok(id) = client.selected().cloned().ok_or(()) else {
        panic!("a session is selected")
    };
    let Ok(()) = client.reconcile(&id) else {
        panic!("baseline reconcile")
    };
    let before = match client.view() {
        Some(view) => view.clone(),
        None => panic!("a view exists"),
    };

    client.peer_mut().set_fault(Fault::CloseStream);
    let Ok(_) = client.pump() else {
        panic!("pump after close")
    };

    let after = match client.view() {
        Some(view) => view.clone(),
        None => panic!("a view survives"),
    };
    assert_eq!(
        after.slices, before.slices,
        "reconnecting re-reads the same durable truth and must not duplicate it"
    );
    assert!(
        client.diagnostics().stream_closes >= 1,
        "the close is counted"
    );
    assert!(
        client.diagnostics().reconciliations >= 2,
        "selecting reconciled once, the close reconciled again"
    );
    assert!(
        client.stream_state().is_live(),
        "the client is live again after reconciling"
    );
}

#[test]
fn a_restart_replays_the_same_session_truth() {
    // The restart law. Live state is gone; durable truth is not.
    let mut client = client();
    let Ok(id) = client.create_session() else {
        panic!("create");
    };
    let Ok(()) = client.submit_prompt("hello") else {
        panic!("prompt");
    };
    let Ok(_) = client.pump() else { panic!("pump") };
    let Ok(()) = client.reconcile(&id) else {
        panic!("baseline reconcile")
    };
    let before = match client.view() {
        Some(view) => view.clone(),
        None => panic!("a view exists"),
    };

    client.peer_mut().restart();
    let Ok(_) = client.pump() else {
        panic!("pump after restart")
    };

    let after = match client.view_of(&id) {
        Some(view) => view.clone(),
        None => panic!("the session survives a restart"),
    };
    let shape = |view: &SessionView| -> Vec<(SliceKind, String)> {
        view.slices
            .iter()
            .map(|s| (s.kind, s.text.clone()))
            .collect()
    };
    assert_eq!(
        shape(&after),
        shape(&before),
        "the same session truth after a restart"
    );
}

#[test]
fn a_live_interrupt_is_kept_as_a_named_observation_not_as_the_outcome() {
    // The terminal refresh reads the committed aborted error. The named
    // live observation survives beside that durable outcome, rather than
    // overriding a newer execution's outcome on a later refresh.
    let mut client = client();
    let Ok(id) = client.create_session() else {
        panic!("create");
    };
    let Ok(()) = client.submit_prompt("hello") else {
        panic!("prompt");
    };
    let Ok(_) = client.pump() else { panic!("pump") };
    let Ok(()) = client.interrupt() else {
        panic!("interrupt")
    };
    let Ok(_) = client.pump() else { panic!("pump") };

    let Some(view) = client.view() else {
        panic!("a session is open")
    };
    let interrupted = view
        .interrupted_observed
        .clone()
        .unwrap_or_else(|| panic!("committed interruption must remain a named observation"));
    assert!(
        matches!(
            view.execution,
            ExecutionState::Settled {
                outcome: Outcome::Interrupted,
                ..
            }
        ),
        "terminal hydration must read the durable interruption"
    );

    // Another rebuild preserves the observation alongside durable truth.
    if let Err(error) = client.reconcile(&id) {
        panic!("reconcile: {error}");
    }
    let Some(view) = client.view() else {
        panic!("a session is open")
    };
    assert_eq!(
        view.interrupted_observed.as_ref(),
        Some(&interrupted),
        "the client still knows what it saw"
    );
}

#[test]
fn a_stale_interrupt_never_resurrects_over_a_later_terminal() {
    // THE MUTANT THIS EXISTS TO KILL.
    //
    // An earlier revision copied an observed interrupt into the rebuilt view
    // whenever that rebuild carried no execution id — and EVERY rebuild carries
    // none, because `apply_durable` sets `execution_id: None` for every
    // assistant finish, a later successful stop included. So the guard matched
    // always.
    //
    // The failure it produced: interrupt execution A, lose the stream, let
    // execution B complete somewhere else, reconcile the newer transcript, and
    // the client would show B's success as A's interrupt. Absence cannot
    // establish sameness, and the protocol offers no way to correlate a live
    // execution with a rebuilt terminal.
    let mut client = client();
    let Ok(id) = client.create_session() else {
        panic!("create");
    };
    let Ok(()) = client.submit_prompt("hello") else {
        panic!("prompt");
    };
    let Ok(_) = client.pump() else { panic!("pump") };
    let Ok(()) = client.interrupt() else {
        panic!("interrupt")
    };
    let Ok(_) = client.pump() else { panic!("pump") };
    assert!(client
        .view()
        .and_then(|view| view.interrupted_observed.clone())
        .is_some());

    // A later turn settles in durable truth, as one completed through another
    // client during a disconnect would.
    let Ok(()) = client.submit_prompt("and again") else {
        panic!("second prompt");
    };
    let Ok(_) = client.pump() else { panic!("pump") };
    if let Err(error) = client.reconcile(&id) {
        panic!("reconcile: {error}");
    }

    let Some(view) = client.view() else {
        panic!("a session is open")
    };
    assert!(
        !matches!(
            view.execution,
            crate::projection::ExecutionState::Settled {
                outcome: crate::projection::Outcome::Interrupted,
                ..
            }
        ),
        "a stale interrupt must not become the outcome of a later execution: {:?}",
        view.execution
    );
}

#[test]
fn a_skipped_durable_sequence_is_not_treated_as_loss() {
    // The client-level counterpart of the cursor's corrected law, and the
    // inversion of what this witness used to assert.
    //
    // It previously withheld a durable sequence and demanded the client call it
    // a gap. The contract says otherwise: within-aggregate sequences are
    // monotonic-WITH-GAPS and must never be assumed contiguous, because `Seq`
    // is a position in the aggregate's durable log rather than a count of what
    // the stream chose to publish. A client that treats a skipped number as
    // loss reconciles on every turn and — as it did against a real product —
    // throws away the live execution terminal each time.
    //
    // Real loss has its own signals, which the transport raises and which are
    // witnessed separately: a closed stream and a refused frame.
    let mut client = client();
    let Ok(_) = client.create_session() else {
        panic!("create");
    };
    let Ok(()) = client.submit_prompt("hello") else {
        panic!("prompt");
    };
    client.peer_mut().set_fault(Fault::SkipDurable(2));
    let Ok(_) = client.pump() else { panic!("pump") };

    assert_eq!(
        client.diagnostics().gaps_observed,
        0,
        "a legal sequence skip is not a gap: {:?}",
        client.diagnostics()
    );
    assert!(
        client.stream_state().is_live(),
        "and it must not cost the client its live baseline: {:?}",
        client.stream_state()
    );
}

#[test]
fn duplicate_durable_frames_are_ignored_rather_than_re_rendered() {
    let mut client = client();
    let Ok(id) = client.create_session() else {
        panic!("create");
    };
    let Ok(()) = client.submit_prompt("hello") else {
        panic!("prompt");
    };
    let Ok(_) = client.pump() else { panic!("pump") };
    let Ok(()) = client.reconcile(&id) else {
        panic!("baseline reconcile")
    };
    let before = match client.view() {
        Some(view) => view.clone(),
        None => panic!("a view exists"),
    };

    // Reconcile again by hand. Every durable row is one the client has seen.
    let Ok(()) = client.reconcile(&id) else {
        panic!("reconcile")
    };
    let after = match client.view() {
        Some(view) => view.clone(),
        None => panic!("a view exists"),
    };
    assert_eq!(
        after.slices, before.slices,
        "re-reading durable truth cannot grow or change the transcript"
    );
}

#[test]
fn interrupting_settles_the_execution_as_interrupted() {
    // Deliverable 2's cancellation affordance, end to end.
    let mut client = client();
    let Ok(_) = client.create_session() else {
        panic!("create");
    };
    let Ok(()) = client.interrupt() else {
        panic!("interrupt")
    };
    let Ok(_) = client.pump() else { panic!("pump") };
    let Some(view) = client.view() else {
        panic!("a view exists")
    };
    assert!(
        matches!(
            view.execution,
            ExecutionState::Settled {
                outcome: Outcome::Interrupted,
                ..
            }
        ),
        "interrupt settles as interrupted, not as success: {:?}",
        view.execution
    );
}

#[test]
fn an_interrupted_turn_then_a_successful_one_survives_a_restart_without_resurrecting_it() {
    // THE END-TO-END SHAPE, which the rebuild-level witnesses do not reach.
    //
    // `an_interrupted_turn_is_not_resurrected_over_a_later_successful_one`
    // proves the FOLD: given two durable records, the newer one wins. It builds
    // those records by hand. This drives the same property through the whole
    // client — a real interrupt on a real execution, a second turn that really
    // succeeds, then a restart that destroys live state and keeps durable truth
    // — because the fold being right does not prove the client reaches it.
    //
    // The restart is what makes this worth having. A live interrupt is held as
    // a NAMED observation, and the danger is that it outlives the execution it
    // named: after a restart the client rebuilds from durable history, and an
    // observation that survived into that rebuild would repaint a finished
    // session as interrupted.
    let mut client = client();
    let Ok(id) = client.create_session() else {
        panic!("create")
    };

    // A: interrupted.
    let Ok(()) = client.submit_prompt("first") else {
        panic!("prompt A")
    };
    let Ok(_) = client.pump() else {
        panic!("pump A")
    };
    let Ok(()) = client.interrupt() else {
        panic!("interrupt A")
    };
    let Ok(_) = client.pump() else {
        panic!("pump after interrupt")
    };
    assert!(
        matches!(
            client.view().map(|view| &view.execution),
            Some(ExecutionState::Settled {
                outcome: Outcome::Interrupted,
                ..
            })
        ),
        "the interrupt took: {:?}",
        client.view().map(|view| &view.execution)
    );

    // B: succeeds.
    let Ok(()) = client.submit_prompt("second") else {
        panic!("prompt B")
    };
    let Ok(_) = client.pump() else {
        panic!("pump B")
    };
    let Ok(()) = client.reconcile(&id) else {
        panic!("reconcile")
    };
    let before = client.view().cloned().unwrap_or_else(|| panic!("a view"));

    // The restart: live state dies, durable truth does not.
    client.peer_mut().restart();
    let Ok(_) = client.pump() else {
        panic!("pump after restart")
    };
    client.peer_mut().resubscribe();
    let Ok(()) = client.reconcile(&id) else {
        panic!("reconcile after restart")
    };

    let after = client
        .view_of(&id)
        .cloned()
        .unwrap_or_else(|| panic!("the session survives a restart"));
    assert!(
        !matches!(
            after.execution,
            ExecutionState::Settled {
                outcome: Outcome::Interrupted,
                ..
            }
        ),
        "the stale interrupt did not survive the rebuild: {:?}",
        after.execution
    );
    assert_eq!(
        after.execution, before.execution,
        "the outcome after the restart is the one durable truth already carried"
    );

    // NO DUPLICATES. A reconcile that re-applied history rather than replacing
    // it would show each turn twice, which is the other half of "without
    // duplicates or resurrected waiters".
    let shape = |view: &SessionView| -> Vec<(SliceKind, String)> {
        view.slices
            .iter()
            .map(|slice| (slice.kind, slice.text.clone()))
            .collect()
    };
    assert_eq!(
        shape(&after),
        shape(&before),
        "the restart replayed the same transcript, not a doubled one"
    );
}

#[test]
fn a_terminal_hydrates_before_consuming_the_next_executions_live_frames() {
    let mut client = client();
    client
        .create_session()
        .unwrap_or_else(|error| panic!("create: {error}"));
    client
        .submit_prompt("first")
        .unwrap_or_else(|error| panic!("first: {error}"));
    client
        .submit_prompt("second")
        .unwrap_or_else(|error| panic!("second: {error}"));
    let before = client.diagnostics().reconciliations;
    client
        .pump()
        .unwrap_or_else(|error| panic!("first pump: {error}"));
    assert_eq!(client.diagnostics().reconciliations, before + 1);
    assert!(
        client.peer_mut().queued_frames() > 0,
        "next execution was consumed before terminal hydration"
    );
    for _ in 0..4 {
        client
            .pump()
            .unwrap_or_else(|error| panic!("continuing pump: {error}"));
    }
    let view = client.view().unwrap_or_else(|| panic!("selected view"));
    for text in ["echo first", "echo second"] {
        assert_eq!(
            view.slices
                .iter()
                .filter(|slice| slice.kind == SliceKind::Text && slice.text == text)
                .count(),
            1,
            "queued execution duplicated or lost {text}"
        );
    }
    assert_eq!(client.diagnostics().stream_closes, 0);
}

#[test]
fn a_quiescence_fact_outlives_a_terminal_only_the_relist_knows() {
    // The Q3 row's shape against the real product: the quiescence edge crossed the
    // client, but the turn's terminal never crossed the live stream — it exists only in
    // the durable re-list, so the view never stamped it. The rebuild carries that
    // terminal, and it must not erase the quiescence fact the client was told: the
    // terminal's silence is not a fresh fact, and the re-subscription's hydration is the
    // recovery path a cut live edge leaves behind.
    let mut client = client();
    let session = client
        .create_session()
        .unwrap_or_else(|error| panic!("create: {error}"));
    client
        .reconcile(&session)
        .unwrap_or_else(|error| panic!("reconcile: {error}"));
    client
        .submit_prompt("hello")
        .unwrap_or_else(|error| panic!("prompt: {error}"));
    // The settle idle edge, as the live stream (or the next hydration) told
    // this client — stamped, with no live terminal ever applied.
    let envelope = EventEnvelope::decode(&format!(
        "{{\"id\":\"evt_status_witness\",\"type\":\"session.status\",\"created\":1791695003000,\
         \"data\":{{\"sessionID\":\"{session}\",\"status\":{{\"type\":\"idle\"}}}}}}",
        session = session.as_inner().as_str()
    ))
    .unwrap_or_else(|error| panic!("status frame: {error}"));
    client
        .views
        .get_mut(&session)
        .unwrap_or_else(|| panic!("reconcile installs the view"))
        .apply(&envelope, Bounds::PRODUCTION)
        .unwrap_or_else(|error| panic!("status fold: {error}"));
    client
        .reconcile(&session)
        .unwrap_or_else(|error| panic!("terminal reconcile: {error}"));
    let view = client
        .view()
        .unwrap_or_else(|| panic!("the selected session has a view"));
    assert!(
        matches!(
            view.execution,
            ExecutionState::Settled {
                outcome: Outcome::Succeeded,
                ..
            }
        ),
        "the durable terminal stands: {:?}",
        view.execution
    );
    assert_eq!(
        view.idle,
        Some(true),
        "the quiescence fact the client was told outlives the terminal the re-list carries"
    );
    assert_eq!(
        view.idle_created,
        Some(1_791_695_003_000),
        "and its stamp travels with it"
    );
}

#[test]
fn a_busy_fact_does_not_outlive_a_terminal_only_the_relist_knows() {
    // The mirror of the row above: the prepare busy for a turn whose terminal
    // crossed only through the durable re-list is stale — the turn is
    // durably over, and carrying the edge would claim work the durable record
    // says ended. Absent is not busy:
    // the screen returns to the honest unreported.
    let mut client = client();
    let session = client
        .create_session()
        .unwrap_or_else(|error| panic!("create: {error}"));
    client
        .reconcile(&session)
        .unwrap_or_else(|error| panic!("reconcile: {error}"));
    client
        .submit_prompt("hello")
        .unwrap_or_else(|error| panic!("prompt: {error}"));
    let envelope = EventEnvelope::decode(&format!(
        "{{\"id\":\"evt_status_witness2\",\"type\":\"session.status\",\"created\":1791695002000,         \"data\":{{\"sessionID\":\"{session}\",\"status\":{{\"type\":\"busy\"}}}}}}",
        session = session.as_inner().as_str()
    ))
    .unwrap_or_else(|error| panic!("status frame: {error}"));
    client
        .views
        .get_mut(&session)
        .unwrap_or_else(|| panic!("reconcile installs the view"))
        .apply(&envelope, Bounds::PRODUCTION)
        .unwrap_or_else(|error| panic!("execution fold: {error}"));
    client
        .reconcile(&session)
        .unwrap_or_else(|error| panic!("terminal reconcile: {error}"));
    let view = client
        .view()
        .unwrap_or_else(|| panic!("the selected session has a view"));
    assert!(
        matches!(view.execution, ExecutionState::Settled { .. }),
        "the durable terminal stands: {:?}",
        view.execution
    );
    assert_eq!(
        view.idle, None,
        "a stale busy yields to the turn the terminal ends"
    );
}

/// A wire peer over the contract double that queues raw live frames, so a
/// witness can tell the client something the double would never emit of its
/// own — a `session.status` edge — in exactly the order it chooses.
#[derive(Debug)]
struct StatusPeer {
    inner: ContractDouble,
    frames: std::collections::VecDeque<FramePoll>,
    closed: bool,
}

impl StatusPeer {
    fn new() -> Self {
        Self {
            inner: ContractDouble::new(),
            frames: std::collections::VecDeque::new(),
            closed: false,
        }
    }

    fn status(&mut self, session: &str, kind: &str, created: u64) {
        let frame = serde_json::json!({
            "id": format!("evt_status_{created}"),
            "type": "session.status",
            "created": created,
            "data": { "sessionID": session, "status": { "type": kind } }
        })
        .to_string();
        self.frames.push_back(FramePoll::Frame(frame));
    }
}

impl Transport for StatusPeer {
    fn call(
        &mut self,
        request: &WireRequest,
    ) -> Result<crate::transport::WireResponse, TransportError> {
        self.inner.call(request)
    }
}

impl Connection for StatusPeer {
    fn subscribed(&self) -> bool {
        !self.closed
    }
    fn poll_frame(&mut self) -> Result<FramePoll, TransportError> {
        let frame = self.frames.pop_front().unwrap_or(FramePoll::Idle);
        if matches!(frame, FramePoll::Closed) {
            self.closed = true;
        }
        Ok(frame)
    }
    fn resubscribe(&mut self) {
        self.closed = false;
    }
}

#[test]
fn a_stream_cut_returns_the_quiescence_fact_to_unreported() {
    // the reviewer SQ-H3: the same client across
    // a server restart must not keep the idle it observed on the dead
    // connection: without a fresh status the footer returns to the honest
    // unreported, and a same-process reconnect stays truthful when a fresh
    // edge arrives.
    let mut client = Client::new(StatusPeer::new(), Bounds::PRODUCTION);
    if let Err(error) = client.select_first_offer() {
        panic!("the double must offer a selection: {error}");
    }
    let Ok(session) = client.create_session() else {
        panic!("create")
    };
    client
        .reconcile(&session)
        .unwrap_or_else(|error| panic!("reconcile: {error}"));

    // The live idle edge the connection carried.
    client
        .connection
        .status(session.as_inner().as_str(), "idle", 1_791_695_003_000);
    let Ok(_) = client.pump() else {
        panic!("pump the idle edge")
    };
    assert_eq!(
        client.view().map(|view| view.idle),
        Some(Some(true)),
        "the live idle edge must be reported"
    );

    // The server restarts: the subscription dies with it.
    client.connection.frames.push_back(FramePoll::Closed);
    let Ok(_) = client.pump() else {
        panic!("pump the cut")
    };
    assert_eq!(
        client.view().and_then(|view| view.idle),
        None,
        "the dead connection's observation must not survive the cut"
    );
    assert!(
        client.stream_state().is_live(),
        "the reconnect re-established the stream; the fact stays unreported"
    );

    // Same-process reconnect, fresh edge: the footer is truthful again.
    client
        .connection
        .status(session.as_inner().as_str(), "busy", 1_791_695_004_000);
    let Ok(_) = client.pump() else {
        panic!("pump the fresh edge")
    };
    assert_eq!(
        client.view().map(|view| view.idle),
        Some(Some(false)),
        "a fresh edge after the reconnect must be reported"
    );
}

#[test]
fn a_fresh_busy_fact_survives_the_relist_of_the_preceding_terminal() {
    // the reviewer SQ-H3: the next preparation's
    // busy, observed live AFTER the relist read the preceding execution's
    // terminal, is newer than that terminal — a further relist must not
    // discard it.
    let mut client = Client::new(StatusPeer::new(), Bounds::PRODUCTION);
    if let Err(error) = client.select_first_offer() {
        panic!("the double must offer a selection: {error}");
    }
    let Ok(session) = client.create_session() else {
        panic!("create")
    };
    client
        .reconcile(&session)
        .unwrap_or_else(|error| panic!("reconcile: {error}"));
    client
        .submit_prompt("review turn")
        .unwrap_or_else(|error| panic!("prompt: {error}"));
    // The turn's terminal crosses only through the durable re-list.
    client
        .reconcile(&session)
        .unwrap_or_else(|error| panic!("terminal reconcile: {error}"));
    assert!(
        matches!(
            client.view().map(|view| &view.execution),
            Some(ExecutionState::Settled { .. })
        ),
        "the durable terminal stands: {:?}",
        client.view().map(|view| &view.execution)
    );

    // The next preparation's busy, observed live after that history read.
    client
        .connection
        .status(session.as_inner().as_str(), "busy", 1_791_695_005_000);
    let Ok(_) = client.pump() else {
        panic!("pump the fresh busy")
    };
    assert_eq!(
        client.view().map(|view| view.idle),
        Some(Some(false)),
        "the fresh busy must be reported"
    );

    // A further relist carrying the same preceding terminal: the fresh busy
    // stands.
    client
        .reconcile(&session)
        .unwrap_or_else(|error| panic!("relist: {error}"));
    assert_eq!(
        client.view().map(|view| view.idle),
        Some(Some(false)),
        "a relist of the preceding terminal must not discard the fresh busy"
    );
}

#[test]
fn a_newer_terminal_clears_stale_busy_at_the_retention_bound() {
    // the reviewer SQ-H3 retention remainder: at
    // the transcript retention bound a completed newer turn evicts the
    // older user rows, so a count of RETAINED rows no longer moves — and
    // the preparation's stale busy of the turn's own preparation survived
    // its own terminal. The folded count names the turn retention cannot.
    let mut bounds = Bounds::PRODUCTION;
    bounds.max_retained_slices = 2;
    let Ok(bounds) = bounds.validate() else {
        panic!("bounds must validate")
    };
    let mut client = Client::new(StatusPeer::new(), bounds);
    if let Err(error) = client.select_first_offer() {
        panic!("the double must offer a selection: {error}");
    }
    let Ok(session) = client.create_session() else {
        panic!("create")
    };
    client
        .reconcile(&session)
        .unwrap_or_else(|error| panic!("reconcile: {error}"));

    // Turn one completes. At the two-slice bound the row that names it is
    // already evicted by the time its terminal is installed.
    client
        .submit_prompt("first completed turn")
        .unwrap_or_else(|error| panic!("prompt: {error}"));
    client
        .reconcile(&session)
        .unwrap_or_else(|error| panic!("terminal reconcile: {error}"));
    let Some(view) = client.view() else {
        panic!("a view exists")
    };
    assert!(
        matches!(view.execution, ExecutionState::Settled { .. }),
        "the durable terminal stands: {:?}",
        view.execution
    );
    assert_eq!(
        view.user_row_count(),
        0,
        "the bound evicted the row that names turn one: the precondition"
    );

    // Turn two's preparation busy is observed live, and its terminal then
    // installs over the re-list while the busy is held.
    client
        .connection
        .status(session.as_inner().as_str(), "busy", 1_791_695_006_000);
    let Ok(_) = client.pump() else {
        panic!("pump the preparation busy")
    };
    assert_eq!(
        client.view().map(|view| view.idle),
        Some(Some(false)),
        "the preparation busy must be reported"
    );
    client
        .submit_prompt("second completed turn")
        .unwrap_or_else(|error| panic!("prompt: {error}"));
    client
        .reconcile(&session)
        .unwrap_or_else(|error| panic!("second terminal reconcile: {error}"));
    assert!(
        matches!(
            client.view().map(|view| &view.execution),
            Some(ExecutionState::Settled { .. })
        ),
        "turn two's terminal stands: {:?}",
        client.view().map(|view| &view.execution)
    );
    assert_eq!(
        client.view().map(|view| view.idle),
        Some(None),
        "the newer terminal must clear the stale busy at the bound"
    );
}

#[test]
fn an_install_ending_on_an_early_failure_replaces_a_live_running_turn() {
    let mut client = client();
    let session = client
        .create_session()
        .unwrap_or_else(|error| panic!("create: {error}"));
    let live = crate::wire::execution_id("exe_live").unwrap_or_else(|error| panic!("{error}"));
    if let Some(view) = client.views.get_mut(&session) {
        view.execution = crate::projection::ExecutionState::Running { execution_id: live };
    }
    let failure_row = crate::wire::MessageWireId::parse("msg_failure".into())
        .unwrap_or_else(|| panic!("failure id"));
    let mut held = Vec::new();
    // Two relists of the same tail: the turn settles once, and the second
    // names the same terminal rather than a newer one.
    for _ in 0..2 {
        let mut rebuilt = SessionView::new(&session);
        for row in [
            crate::wire::DurableMessage::User { text: "go".into() },
            crate::wire::DurableMessage::ExecutionFailure {
                text: "context could not be prepared".into(),
            },
        ] {
            rebuilt
                .apply_durable(&row, Bounds::PRODUCTION)
                .unwrap_or_else(|error| panic!("{error}"));
        }
        client
            .install_history(
                &session,
                rebuilt,
                Some((failure_row.clone(), crate::projection::Outcome::Failed)),
                history::OlderHistory::Available,
                0,
            )
            .unwrap_or_else(|error| panic!("install: {error}"));
        let view = client.view().unwrap_or_else(|| panic!("view"));
        assert!(
            matches!(
                view.execution,
                crate::projection::ExecutionState::Settled {
                    outcome: crate::projection::Outcome::Failed,
                    ..
                }
            ),
            "a live Running survived a failure tail: {:?}",
            view.execution
        );
        held.push(view.held_terminal.clone());
    }
    assert_eq!(held[0], held[1], "the same tail is no newer terminal");
    assert!(held[0].is_some());
}

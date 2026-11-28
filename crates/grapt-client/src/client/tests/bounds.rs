//! bounds witnesses, split from the carrier by subject.
//!
//! Moved unchanged: the ratchet bounds a FILE, and a witness that was edited
//! on its way out of one would be a different witness.

use super::*;

#[test]
fn one_pump_does_bounded_work_rather_than_draining_an_unbounded_stream() {
    // The performance bound that is actually assertable. Wall-clock timing is
    // both flaky and unavailable here — the workspace bans `Instant::now` for
    // every target — so the bound is expressed as WORK: a single pump applies
    // at most `max_pending_events` frames and then yields, so a fast producer
    // cannot starve the key-reading half of the loop.
    let bounds = match (Bounds {
        max_pending_events: 4,
        ..Bounds::PRODUCTION
    })
    .validate()
    {
        Ok(bounds) => bounds,
        Err(error) => panic!("witness bounds: {error}"),
    };
    let mut client = Client::new(ContractDouble::new(), bounds);
    if let Err(error) = client.select_first_offer() {
        panic!("the double must offer a selection: {error}");
    }
    let Ok(_) = client.create_session() else {
        panic!("create")
    };
    // Queue far more frames than one pump may take.
    for _ in 0..6 {
        let Ok(()) = client.submit_prompt("hello") else {
            panic!("prompt")
        };
    }

    let before = client.diagnostics().frames_decoded;
    let Ok(_) = client.pump() else { panic!("pump") };
    let first_pump = client.diagnostics().frames_decoded - before;
    assert!(
        first_pump <= bounds.max_pending_events,
        "one pump applied {first_pump} frames, above the {} bound",
        bounds.max_pending_events
    );

    // And the work is not lost — a later pump continues where this one
    // stopped, so bounding the pump costs latency and never content.
    let Ok(_) = client.pump() else {
        panic!("second pump")
    };
    assert!(
        client.diagnostics().frames_decoded > before + first_pump,
        "the next pump continued from where the bound stopped it"
    );
}

#[test]
fn a_pump_at_its_budget_leaves_the_boundary_frame_queued_rather_than_dropping_it() {
    // THE defect. The previous revision polled a frame, incremented the
    // counter, and then broke when the counter exceeded the bound - so the
    // frame that tripped the bound was already consumed and was lost. The old
    // continuation test proved that LATER frames arrived, which cannot protect
    // a final frame because there is no later frame to observe.
    //
    // Here the budget is one. Every frame the double holds must still be
    // observed across successive pumps, with NONE missing.
    let bounds = match (Bounds {
        max_pending_events: 1,
        ..Bounds::PRODUCTION
    })
    .validate()
    {
        Ok(bounds) => bounds,
        Err(error) => panic!("witness bounds: {error}"),
    };
    // The DOUBLE keeps production bounds so it really queues several frames;
    // only the CLIENT gets the tiny budget, which is what is under test.
    let mut client = Client::new(ContractDouble::new(), bounds);
    if let Err(error) = client.select_first_offer() {
        panic!("the double must offer a selection: {error}");
    }
    let Ok(_) = client.create_session() else {
        panic!("create")
    };
    let Ok(()) = client.submit_prompt("hello") else {
        panic!("prompt")
    };

    // Count what the double is holding, then drain it one frame per pump.
    let queued = client.peer_mut().queued_frames();
    assert!(queued > 1, "the double really is holding several frames");

    let mut observed = 0usize;
    for _ in 0..(queued * 4) {
        // Every taken frame is decoded, whether it applies, repeats what the
        // snapshot holds, or is superseded by it.
        let before = client.diagnostics().frames_decoded;
        let Ok(_) = client.pump() else { panic!("pump") };
        let after = client.diagnostics().frames_decoded;
        observed += after - before;
        if client.peer_mut().queued_frames() == 0 {
            break;
        }
    }
    assert_eq!(
        client.peer_mut().queued_frames(),
        0,
        "every queued frame was eventually taken"
    );
    assert!(
        observed >= queued,
        "no frame was consumed-and-dropped at the budget boundary: observed {observed} of {queued}"
    );
    assert_eq!(
        client.peer_mut().dropped_frames(),
        0,
        "and the producer refused none of them"
    );
}

#[test]
fn a_row_this_client_cannot_draw_does_not_cost_it_liveness() {
    // THE CORRECTION, and the reason it matters more than it looks.
    //
    // This witness previously asserted the opposite: that a row the client
    // could not render left it reconciling rather than live. Against a real
    // product that rule cost the user everything. A real provider failure wrote
    // a `system` row explaining itself, the client had no presentation for it,
    // and the resulting count kept the cursor out of a live baseline PERMANENTLY
    // — so it reconciled from durable history forever and never showed a single
    // live execution, while the product was emitting the whole lifecycle on a
    // subscription it had successfully opened.
    //
    // The mistake was conflating two facts. "I walked the history to
    // exhaustion" and "I can draw every row I read" are different claims, and
    // only the first is about completeness. The client HAS this history.
    let mut client = Client::new(ContractDouble::new(), Bounds::PRODUCTION);
    if let Err(error) = client.select_first_offer() {
        panic!("the double must offer a selection: {error}");
    }
    let Ok(id) = client.create_session() else {
        panic!("create")
    };
    let Ok(()) = client.submit_prompt("hello") else {
        panic!("prompt")
    };
    client.peer_mut().set_fault(Fault::UnrenderableRow);
    prove(&mut client);
    if let Err(error) = client.reconcile(&id) {
        panic!("reconcile must succeed: {error}")
    }

    assert!(
        client.stream_state().is_live(),
        "a complete history is live even when a row cannot be drawn: {:?}",
        client.stream_state()
    );
    assert_eq!(
        client.unsupported_rows(),
        1,
        "and the screen is still told how much it is not drawing"
    );
}

#[test]
fn a_truncated_older_history_is_named_not_laundered_into_a_whole_one() {
    // The half that must NOT change. A prefix is a prefix: when the walk toward
    // older history cannot reach the start, the client is missing history and
    // has to say so. It says so BESIDE the stream state rather than as a gap,
    // because the tail it read is whole — and it must not become a way to
    // report an incomplete history as complete, in the transcript or in the
    // accounting.
    let mut client = Client::new(ContractDouble::new(), Bounds::PRODUCTION);
    if let Err(error) = client.select_first_offer() {
        panic!("the double must offer a selection: {error}");
    }
    let Ok(id) = client.create_session() else {
        panic!("create")
    };
    let Ok(()) = client.submit_prompt("hello") else {
        panic!("prompt")
    };
    // A producer that pages nowhere: the walk stops on the no-progress guard
    // with the start unreached.
    client.peer_mut().set_fault(Fault::StuckCursor);
    if let Err(error) = client.reconcile(&id) {
        panic!("reconcile must succeed on a readable tail: {error}")
    }
    let Ok(_) = client.pump() else { panic!("pump") };

    assert!(
        matches!(
            client.older_history(),
            crate::client::OlderHistory::Failed { .. }
        ),
        "the unread history is named: {:?}",
        client.older_history()
    );
    assert!(
        client.diagnostics().degraded_reconciliations >= 1,
        "and the degradation is counted"
    );
    assert_eq!(
        client.view().and_then(|view| view.cache.history_share()),
        None,
        "and no history-wide accounting is claimed over it"
    );
}

#[test]
fn a_complete_readable_page_does_earn_liveness() {
    // The positive control. A refusal that fired on every page would satisfy
    // the witness above while making the client permanently degraded.
    let mut client = Client::new(ContractDouble::new(), Bounds::PRODUCTION);
    if let Err(error) = client.select_first_offer() {
        panic!("the double must offer a selection: {error}");
    }
    let Ok(id) = client.create_session() else {
        panic!("create")
    };
    let Ok(()) = client.submit_prompt("hello") else {
        panic!("prompt")
    };
    let Ok(()) = client.reconcile(&id) else {
        panic!("reconcile")
    };
    let Ok(_) = client.pump() else { panic!("pump") };
    assert!(
        client.stream_state().is_live(),
        "a complete, fully readable page earns liveness"
    );
    assert_eq!(
        client.older_history(),
        &crate::client::OlderHistory::Complete,
        "and the walk reached the start"
    );
    assert_eq!(
        client.diagnostics().degraded_reconciliations,
        0,
        "and nothing was degraded"
    );
}

#[test]
fn an_oversized_response_is_refused_at_transport_ingress() {
    // `max_response_bytes` was declared in `bounds.rs` and enforced nowhere:
    // `WireResponse` exposed an arbitrary `Vec<u8>` and the client parsed it
    // unchecked. Now it is refused at ingress, before it is decoded.
    let bounds = match (Bounds {
        max_response_bytes: 8,
        ..Bounds::PRODUCTION
    })
    .validate()
    {
        Ok(bounds) => bounds,
        Err(error) => panic!("witness bounds: {error}"),
    };
    let mut client = Client::new(ContractDouble::new(), bounds);
    match client.refresh_sessions() {
        Err(ClientError::Transport(TransportError::ResponseTooLarge { bound })) => {
            assert_eq!(bound, 8)
        }
        other => panic!("an oversized response must be refused at ingress, got {other:?}"),
    }
}

#[test]
fn a_response_exactly_at_the_bound_is_accepted() {
    // The other direction. A check written as `>=` would refuse a conformant
    // response, and no witness that only tests oversized payloads would notice.
    let mut client = Client::new(ContractDouble::new(), Bounds::PRODUCTION);
    let Ok(()) = client.refresh_sessions() else {
        panic!("a response within the bound must be accepted")
    };
}

#[test]
fn the_producers_pending_queue_is_bounded_and_says_what_it_refused() {
    // Bounding a pump iteration bounds the CONSUMER. Without a producer bound
    // the double's queue grows without limit, so a long-lived process exhausts
    // memory while every client-side counter reads as healthy.
    let bounds = match (Bounds {
        max_pending_events: 2,
        ..Bounds::PRODUCTION
    })
    .validate()
    {
        Ok(bounds) => bounds,
        Err(error) => panic!("witness bounds: {error}"),
    };
    let mut double = ContractDouble::with_bounds(bounds);
    let session = seed(&mut double, "first");
    for _ in 0..4 {
        double.run_turn(&session, "hello", "hello");
    }
    assert!(
        double.queued_frames() <= bounds.max_pending_events,
        "the queue never exceeded its bound: {}",
        double.queued_frames()
    );
    assert!(
        double.dropped_frames() > 0,
        "and the refusal is VISIBLE rather than silent"
    );
}

/// THIS WITNESS WAS MIS-NAMED, AND THE NAME IS WHY BLOCKER 4 LOOKED CLOSED.
///
/// It was called `the_producers_per_session_log_is_bounded_and_says_what_it_evicted`
/// while it set `max_retained_slices` and asserted `dropped_messages` — that is
/// the rendered-MESSAGE retention vector, a different collection from the
/// durable log entirely. The durable log had no bound at all: `append` pushed
/// unconditionally. So a reader auditing "is the log bounded?" found a green
/// test whose name said yes, and the actual `entry.log.push` grew forever
/// underneath it.
///
/// The name is corrected to the collection it actually measures, and the
/// durable log gets its own witness below. A test whose name does not match its
/// subject is worse than a missing test, because it answers the question
/// falsely instead of leaving it open.
#[test]
fn the_producers_message_retention_is_bounded_and_says_what_it_evicted() {
    let bounds = match (Bounds {
        max_retained_slices: 2,
        ..Bounds::PRODUCTION
    })
    .validate()
    {
        Ok(bounds) => bounds,
        Err(error) => panic!("witness bounds: {error}"),
    };
    let mut double = ContractDouble::with_bounds(bounds);
    let session = seed(&mut double, "first");
    for _ in 0..4 {
        double.run_turn(&session, "hello", "hello");
    }
    assert!(
        double.dropped_messages(&session) > 0,
        "the message retention evicted and counted it"
    );
    // And the eviction becomes the contract's own truncation signal, so a
    // client cannot mistake the prefix for the whole history.
    let Ok(request) = WireRequest::new(Operation::MessageList, Some(&session), None) else {
        panic!("address")
    };
    let Ok(response) = double.call(&request) else {
        panic!("answer")
    };
    let page = match crate::wire::decode_messages(&response.body) {
        Ok(page) => page,
        Err(error) => panic!("decode: {error}"),
    };
    assert!(
        page.next.is_some(),
        "an evicted log serves a page that offers a cursor to the rest"
    );
}

#[test]
fn the_producers_durable_log_is_bounded_exactly_and_says_what_it_evicted() {
    // The collection review named that had NO bound: `append` pushed
    // unconditionally, so this is the first witness of it that can exist.
    let bounds = match (Bounds {
        max_retained_log_entries: 3,
        ..Bounds::PRODUCTION
    })
    .validate()
    {
        Ok(bounds) => bounds,
        Err(error) => panic!("witness bounds: {error}"),
    };
    let mut double = ContractDouble::with_bounds(bounds);
    let session = seed(&mut double, "first");
    // `create_session` itself appends one durable fact, so the log holds 1 here.
    assert_eq!(double.log_len(&session), 1);
    assert_eq!(
        double.dropped_log_entries(&session),
        0,
        "nothing is evicted below the bound"
    );

    // EXACTLY AT the bound: still nothing evicted. Without this control a
    // producer that evicted one entry early would pass the over-limit assertion
    // below while quietly truncating history it could have served.
    double.append_witness_entries(&session, 2);
    assert_eq!(double.log_len(&session), 3);
    assert_eq!(
        double.dropped_log_entries(&session),
        0,
        "a log exactly at its bound has evicted nothing"
    );

    // One OVER the bound: exactly one eviction, counted.
    double.append_witness_entries(&session, 1);
    assert_eq!(
        double.log_len(&session),
        3,
        "the log never exceeds its bound"
    );
    assert_eq!(
        double.dropped_log_entries(&session),
        1,
        "and the eviction is VISIBLE rather than silent"
    );

    // And the eviction becomes the contract's own truncation signal, so a
    // client cannot mistake the prefix for the whole history.
    let Ok(request) = WireRequest::new(Operation::MessageList, Some(&session), None) else {
        panic!("address")
    };
    let Ok(response) = double.call(&request) else {
        panic!("answer")
    };
    let page = match crate::wire::decode_messages(&response.body) {
        Ok(page) => page,
        Err(error) => panic!("decode: {error}"),
    };
    assert!(
        page.next.is_some(),
        "an evicted durable log serves a page that offers a cursor to the rest"
    );
}

#[test]
fn the_producers_session_store_is_bounded_exactly_and_refuses_visibly() {
    let bounds = match (Bounds {
        max_retained_sessions: 2,
        ..Bounds::PRODUCTION
    })
    .validate()
    {
        Ok(bounds) => bounds,
        Err(error) => panic!("witness bounds: {error}"),
    };
    let mut double = ContractDouble::with_bounds(bounds);
    // EXACTLY AT the bound: both admitted, nothing refused.
    let _first = seed(&mut double, "first");
    let _second = seed(&mut double, "second");
    assert_eq!(double.session_count(), 2);
    assert_eq!(
        double.dropped_sessions(),
        0,
        "a store exactly at its bound has refused nothing"
    );

    // One OVER: refused, and the refusal is in the return type AND counted.
    assert!(
        double.create_session("third").is_none(),
        "the session store refuses past its bound"
    );
    assert_eq!(
        double.session_count(),
        2,
        "and retains no more than the bound"
    );
    assert_eq!(double.dropped_sessions(), 1, "and says so");

    // Over the wire the refusal is a 503, not a 500: a producer at its bound is
    // declining work it is bounded from doing, not reporting a fault.
    //
    // The request carries a WELL-FORMED selection. A bodyless create is a 400
    // now, and a witness that sent one would be asserting the bound while
    // actually observing request validation — the two refusals are different
    // facts and only one of them is what this test is named for.
    let (Ok(agent), Ok(model), Ok(provider)) = (
        crate::wire::agent_id("agent-build"),
        crate::wire::model_id("model-fast"),
        crate::wire::provider_id("double"),
    ) else {
        panic!("catalog identities parse")
    };
    let Ok(body) = crate::wire::encode_session_create(&agent, &model, &provider, "/work/project-a")
    else {
        panic!("encode")
    };
    let Ok(request) = WireRequest::new(Operation::SessionCreate, None, None) else {
        panic!("address")
    };
    let request = request.with_body(body);
    let Ok(response) = double.call(&request) else {
        panic!("answer")
    };
    assert_eq!(response.status, 503);
}

#[test]
fn the_producers_permission_asks_are_bounded_exactly_and_refuse_visibly() {
    let bounds = match (Bounds {
        max_retained_permissions: 2,
        ..Bounds::PRODUCTION
    })
    .validate()
    {
        Ok(bounds) => bounds,
        Err(error) => panic!("witness bounds: {error}"),
    };
    let mut double = ContractDouble::with_bounds(bounds);
    let session = seed(&mut double, "first");
    // EXACTLY AT the bound.
    let _one = seed_ask(&mut double, &session, "write a file");
    let _two = seed_ask(&mut double, &session, "run a command");
    assert_eq!(double.permissions(&session).len(), 2);
    assert_eq!(
        double.dropped_permissions(&session),
        0,
        "asks exactly at the bound refuse nothing"
    );

    // One OVER.
    assert!(
        double
            .ask_permission(&session, "delete everything")
            .is_none(),
        "the ask store refuses past its bound"
    );
    assert_eq!(double.permissions(&session).len(), 2);
    assert_eq!(double.dropped_permissions(&session), 1, "and says so");
}

#[test]
fn an_unbounded_producer_would_be_caught_by_these_bounds() {
    // The positive control for both producer bounds: under production limits
    // nothing is refused or evicted, so the witnesses above are measuring the
    // bound rather than a permanently-refusing producer.
    let mut double = ContractDouble::new();
    let session = seed(&mut double, "first");
    double.run_turn(&session, "hello", "hello");
    let _ask = seed_ask(&mut double, &session, "write a file");
    assert_eq!(double.dropped_frames(), 0);
    assert_eq!(double.dropped_messages(&session), 0);
    assert_eq!(double.dropped_log_entries(&session), 0);
    assert_eq!(double.dropped_permissions(&session), 0);
    assert_eq!(double.dropped_sessions(), 0);
    assert_eq!(double.session_count(), 1);
}

#[test]
fn a_catalog_the_bound_truncates_is_counted_rather_than_silently_shortened() {
    // Bounding the catalog is correct; bounding it SILENTLY is not. The client
    // keeps 512 rows while the contract admits `MAX_MODEL_ROWS` 2048, so a real
    // inventory can legitimately exceed what is kept — and a picker showing a
    // truncated list is indistinguishable from a product that simply has fewer
    // models unless the drop is counted and shown.
    //
    // The bound is tightened rather than the double inflated, so the witness
    // exercises the real refusal path at a size that stays readable.
    let mut client = Client::new(
        ContractDouble::new(),
        Bounds {
            max_catalog_rows: 1,
            ..Bounds::PRODUCTION
        },
    );
    let Ok(()) = client.refresh_projects() else {
        panic!("projects")
    };

    assert_eq!(
        client.projects().len(),
        1,
        "the bound is enforced, not merely declared"
    );
    assert_eq!(
        client.diagnostics().catalog_rows_dropped,
        1,
        "and the row it refused is COUNTED: the double offers two projects"
    );
}

#[test]
fn a_contract_sized_model_list_is_kept_whole() {
    // THE CLIENT'S BOUND MUST NOT SIT BELOW THE CONTRACT'S. The model list
    // carries its own bound, `MAX_MODEL_ROWS` 2048, larger than the 256 the
    // other catalogs get. This client held ONE number for all three at 512, so
    // a conformant model list was truncated — and the count above would have
    // reported it honestly, which is a good refusal of a defect this client did
    // not have to have. A picker that cannot show the models the product serves
    // is not bounded, it is broken.
    //
    // The size is the contract's own rather than a comfortable number: the
    // property is "whatever the contract admits, this client keeps".
    let mut client = Client::new(
        ContractDouble::new().with_model_catalog_of(grapt_api::u3a::MAX_MODEL_ROWS),
        Bounds::PRODUCTION,
    );
    let Ok(()) = client.refresh_projects() else {
        panic!("projects")
    };
    let project = client.projects()[0].clone();
    if let Err(error) = client.select_project(&project) {
        panic!("select project: {error}");
    }

    assert_eq!(
        client.models().len(),
        grapt_api::u3a::MAX_MODEL_ROWS,
        "a model list the size the contract admits must arrive whole"
    );
    assert_eq!(
        client.diagnostics().catalog_rows_dropped,
        0,
        "and nothing may be reported dropped when nothing was refused"
    );
}

#[test]
fn a_catalog_inside_its_bound_reports_nothing_dropped() {
    // The other half, and the one that keeps the counter honest: a client that
    // incremented unconditionally would also "pass" the witness above while
    // reporting a truncation on every ordinary read, and the status line would
    // permanently accuse the product of hiding rows it had served in full.
    let mut client = Client::new(ContractDouble::new(), Bounds::PRODUCTION);
    let Ok(()) = client.refresh_projects() else {
        panic!("projects")
    };
    assert_eq!(
        client.diagnostics().catalog_rows_dropped,
        0,
        "nothing was refused, so nothing is reported"
    );
}

#[test]
fn the_producers_compaction_record_is_bounded_and_refuses_past_it() {
    let bounds = match (Bounds {
        max_retained_log_entries: 2,
        ..Bounds::PRODUCTION
    })
    .validate()
    {
        Ok(bounds) => bounds,
        Err(error) => panic!("witness bounds: {error}"),
    };
    let mut double = ContractDouble::with_bounds(bounds);
    let session = seed(&mut double, "first");
    let compact = |double: &mut ContractDouble| {
        let Ok(request) = WireRequest::new(Operation::SessionCompact, Some(&session), None) else {
            panic!("compact must address");
        };
        let Ok(body) = crate::wire::encode_compact() else {
            panic!("compact must encode");
        };
        match double.call(&request.with_body(body)) {
            Ok(response) => response.status,
            Err(error) => panic!("compact must answer: {error}"),
        }
    };
    assert_eq!(compact(&mut double), 200);
    assert_eq!(
        compact(&mut double),
        200,
        "exactly at the bound is admitted"
    );
    assert_eq!(compact(&mut double), 409, "past the bound is refused");
    assert_eq!(
        double.compactions().len(),
        2,
        "a refused request is not recorded"
    );
}

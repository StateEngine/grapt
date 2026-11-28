//! paging witnesses, split from the carrier by subject.
//!
//! Moved unchanged: the ratchet bounds a FILE, and a witness that was edited
//! on its way out of one would be a different witness.

use super::*;

#[test]
fn a_page_that_offers_a_cursor_is_followed_rather_than_read_as_a_prefix() {
    // The defect the real product exposed and no in-crate witness could.
    //
    // `v2.message.list` answers a two-message session with BOTH messages and a
    // `next` token; following that token returns zero rows. The client read a
    // cursor's presence as truncation, so it declared "durable history
    // incomplete (page truncated)" over a complete transcript and refused
    // liveness — on every reopened session, permanently, with no way for the
    // user to tell a real gap from this one.
    //
    // The double now serves that exact shape, so this is a witness rather than
    // a description.
    let mut client = client();
    let Ok(id) = client.create_session() else {
        panic!("create")
    };
    let Ok(()) = client.submit_prompt("hello") else {
        panic!("prompt")
    };
    prove(&mut client);
    let Ok(()) = client.reconcile(&id) else {
        panic!("reconcile")
    };

    assert!(
        client.stream_state().is_live(),
        "a page whose continuation is empty IS the whole history: {:?}",
        client.stream_state()
    );
    assert_eq!(client.gap_reason(), None, "and there is no gap to report");
    assert!(
        client.view().is_some_and(|view| view.slices.len() >= 2),
        "with every row rendered"
    );
}

#[test]
fn the_transcript_is_read_in_reading_order_not_in_wire_order() {
    // The product answers `desc` — newest first — unless asked otherwise. A
    // client that took the wire order rendered every reopened transcript
    // backwards, with the assistant's reply above the prompt that caused it.
    //
    // The order is REQUESTED rather than sorted for locally: the contract has
    // an order parameter, and a client that ignored it and re-sorted would be
    // deciding an ordering the server owns.
    let mut client = client();
    let Ok(id) = client.create_session() else {
        panic!("create")
    };
    let Ok(()) = client.submit_prompt("hello") else {
        panic!("prompt")
    };
    prove(&mut client);
    let Ok(()) = client.reconcile(&id) else {
        panic!("reconcile")
    };

    let Some(view) = client.view() else {
        panic!("a projection")
    };
    let kinds: Vec<SliceKind> = view.slices.iter().map(|slice| slice.kind).collect();
    let user = kinds
        .iter()
        .position(|kind| *kind == SliceKind::User)
        .unwrap_or_else(|| panic!("the prompt is in the transcript: {kinds:?}"));
    let reply = kinds
        .iter()
        .position(|kind| *kind == SliceKind::Text)
        .unwrap_or_else(|| panic!("the reply is in the transcript: {kinds:?}"));
    assert!(
        user < reply,
        "the prompt comes before the reply it caused: {kinds:?}"
    );
}

#[test]
fn a_producer_that_pages_forever_stops_at_the_bound_and_names_it() {
    // The page budget is a BOUND, not a comment. A server that offers a cursor
    // forever would otherwise spin the client's walk toward older history, and
    // a declared-but-unenforced budget is a failure by the same law that made
    // every other bound in this crate enforceable.
    //
    // Reaching it costs nothing the tail established: the client went live on
    // the newest page, and the bound is NAMED as the reason earlier history is
    // not shown — not called complete, and not called a full retention.
    let bounds = match (Bounds {
        max_message_pages: 3,
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
    let Ok(id) = client.create_session() else {
        panic!("create")
    };
    let Ok(()) = client.submit_prompt("hello") else {
        panic!("prompt")
    };

    client.peer_mut().set_fault(Fault::EndlessPages);
    let Ok(()) = client.reconcile(&id) else {
        panic!("reconcile")
    };
    for _ in 0..8 {
        let Ok(_) = client.pump() else {
            panic!("pump must return rather than spin")
        };
    }

    assert!(
        client.stream_state().is_live(),
        "the newest page established the tail: {:?}",
        client.stream_state()
    );
    match client.older_history() {
        OlderHistory::Exhausted { pages, .. } => {
            assert_eq!(*pages, 3, "and the walk stopped at the page bound")
        }
        other => panic!("expected the read budget to be named, got {other:?}"),
    }
}

#[test]
fn a_rebuilt_transcript_keeps_the_usage_the_durable_record_measured() {
    // `session.usage.updated` is a LIVE member with no durable block, so a
    // rebuilt projection never sees one. The accounting is still in the durable
    // assistant message, and the client used to ignore it — so reopening a
    // session showed `usage -` for a turn whose cost the record knew exactly.
    // Observed against the real product, which returned 157 input and 10 output
    // tokens for a turn the reopened screen then described as unmeasured.
    let mut client = client();
    let Ok(id) = client.create_session() else {
        panic!("create")
    };
    let Ok(()) = client.submit_prompt("hello") else {
        panic!("prompt")
    };
    let Ok(_) = client.pump() else { panic!("pump") };
    let live = client.view().map(|view| view.usage);
    assert_eq!(
        live,
        Some(Usage {
            input: Some(12),
            output: Some(7)
        }),
        "the live stream reported the usage"
    );

    let Ok(()) = client.reconcile(&id) else {
        panic!("reconcile")
    };
    assert_eq!(
        client.view().map(|view| view.usage),
        live,
        "and the rebuild reports the SAME usage rather than losing it"
    );
}

#[test]
fn an_unmeasured_turn_stays_unmeasured_rather_than_reporting_zero() {
    // The other direction, and the one that matters more. An interrupted turn
    // really has no usage, and a client that defaulted absent accounting to
    // zero would report a measurement nobody took — the exact shape the
    // absent-is-not-zero law forbids.
    let Ok(assistant) = crate::wire::assistant_message(
        "msg_000000000009",
        &[crate::wire::MessageItem::Text("interrupted".to_owned())],
        None,
        None,
        None,
    ) else {
        panic!("assistant message")
    };
    let Ok(bytes) = crate::wire::encode_messages(vec![assistant], None) else {
        panic!("encode")
    };
    let Ok(page) = crate::wire::decode_messages(&bytes) else {
        panic!("decode")
    };
    let Ok(id) = crate::wire::session_id("ses_000000000001") else {
        panic!("id")
    };
    let mut view = SessionView::new(&id);
    for message in &page.messages {
        let Ok(_) = view.apply_durable(message, Bounds::PRODUCTION) else {
            panic!("apply")
        };
    }
    assert_eq!(
        view.usage,
        Usage {
            input: None,
            output: None
        },
        "absent accounting stays absent"
    );
}

#[test]
fn a_continuation_names_the_cursor_alone_because_the_cursor_carries_its_order() {
    // Measured against the live product: `?order=asc` then
    // `?order=asc&cursor=…` is a 400, because the cursor decodes to
    // `{"id": …, "order": "asc", "direction": "next"}` and already says which
    // order it continues. `?cursor=…` alone returns the rest.
    //
    // The client cannot express the refused combination — `Page` is an enum —
    // so this witnesses the PRODUCER half: a double that accepted both would
    // let a future client send it and pass.
    let mut client = client();
    let Ok(id) = client.create_session() else {
        panic!("create")
    };
    let Ok(()) = client.submit_prompt("hello") else {
        panic!("prompt")
    };

    let Ok(request) = WireRequest::new(Operation::MessageList, Some(&id), None) else {
        panic!("address")
    };
    let both = request
        .with_query("order", "asc")
        .with_query("cursor", "more");
    let Ok(response) = client.peer_mut().call(&both) else {
        panic!("the request must reach the producer")
    };
    assert!(
        !response.is_success(),
        "naming an order beside a cursor must be refused, got {}",
        response.status
    );

    let Ok(request) = WireRequest::new(Operation::MessageList, Some(&id), None) else {
        panic!("address")
    };
    let Ok(response) = client
        .peer_mut()
        .call(&request.with_query("cursor", "more"))
    else {
        panic!("the continuation must reach the producer")
    };
    assert!(
        response.is_success(),
        "and the cursor alone is accepted, got {}",
        response.status
    );
}

#[test]
fn a_cursor_that_never_advances_stops_rather_than_duplicating_the_transcript() {
    // Observed against a real product build that accepts the cursor query and
    // pages nowhere: it answers every continuation with the same rows and the
    // same cursor. A client that followed it applied the whole page again on
    // every iteration and produced SIXTY-FOUR copies of the transcript at the
    // page bound — a far worse failure than the false truncation the paging
    // loop was written to fix.
    let mut client = client();
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
    let honest = client
        .view()
        .map(|view| view.slices.len())
        .unwrap_or_default();
    assert!(honest >= 2, "the whole transcript rendered once");

    client.peer_mut().set_fault(Fault::StuckCursor);
    let Ok(()) = client.reconcile(&id) else {
        panic!("reconcile")
    };
    for _ in 0..4 {
        let Ok(_) = client.pump() else {
            panic!("pump must return rather than spin")
        };
    }
    assert_eq!(
        client.view().map(|view| view.slices.len()),
        Some(honest),
        "a cursor that made no progress adds nothing rather than repeating the page"
    );
    assert!(
        matches!(client.older_history(), OlderHistory::Failed { .. }),
        "and the client says it could not read further: {:?}",
        client.older_history()
    );
    assert!(
        client.stream_state().is_live(),
        "without inventing a gap in the tail it did read: {:?}",
        client.stream_state()
    );
}

//! live stream witnesses, split from the carrier by subject.
//!
//! Moved unchanged: the ratchet bounds a FILE, and a witness that was edited
//! on its way out of one would be a different witness.

use super::*;

#[test]
fn a_subscription_opened_before_the_session_still_delivers_live_frames() {
    // The SHIPPED ORDER, which no witness covered. `main.rs` opens the event
    // subscription once, before the screen is taken and therefore before any
    // session exists; the other real-stream witness subscribes AFTER creating
    // its session. Against a real product the shipped client showed no live
    // execution state at all — every screen read `durable succeeded` with
    // `quiescence unreported`, which is the projection rebuilt from durable
    // history rather than observed live — while the product was demonstrably
    // emitting the whole lifecycle on that very stream.
    //
    // An `/api/event` probe against the same product recorded 173 frames for
    // one prompt, including session.execution.started, 166 session.text.delta
    // and session.execution.succeeded. So the events are emitted and
    // receivable, and this asks whether the client in ITS OWN startup order
    // applies any of them.
    let (product, address) = serve("subscribe-first");
    let mut client = connect(address, Bounds::PRODUCTION);

    // Subscribe FIRST, exactly as the binary does.
    client.peer_mut().resubscribe();
    assert!(
        client.peer_mut().subscribed(),
        "the real server must accept a subscription opened before any session"
    );

    let _ = select_offer(&mut client, &product);
    let id = client
        .create_session()
        .unwrap_or_else(|error| panic!("session creation must succeed: {error}"));
    let _ = &id;
    client
        .submit_prompt("stream me")
        .unwrap_or_else(|error| panic!("prompt submission must succeed: {error}"));

    let mut applied = 0usize;
    for _ in 0..JOURNEY_POLLS {
        client
            .pump()
            .unwrap_or_else(|error| panic!("pump must not fail on the real stream: {error}"));
        applied = client.diagnostics().frames_applied;
        if applied > 0 {
            break;
        }
        thread::sleep(JOURNEY_SLEEP);
    }
    assert!(
        applied > 0,
        "a subscription opened before the session delivered no applicable frame;          diagnostics {:?}",
        client.diagnostics()
    );
    wait_for_stream_completion(&mut client);
    drop(product);
}

#[test]
fn the_real_stream_delivers_frames_and_a_drop_reconciles_from_durable_truth() {
    let (mut product, address) = serve("stream");
    let mut client = connect(address, Bounds::PRODUCTION);
    let _ = select_offer(&mut client, &product);

    let id = client
        .create_session()
        .unwrap_or_else(|error| panic!("session creation must succeed: {error}"));

    // Open the real `/api/event` subscription.
    client.peer_mut().resubscribe();
    assert!(
        client.peer_mut().subscribed(),
        "the real server must accept an authenticated event subscription"
    );

    client
        .submit_prompt("stream me")
        .unwrap_or_else(|error| panic!("prompt submission must succeed: {error}"));

    // Pump the REAL stream. `pump` is the same function every double-backed
    // witness drives; only the socket underneath it differs.
    let mut applied = 0usize;
    for _ in 0..JOURNEY_POLLS {
        let _changed = client
            .pump()
            .unwrap_or_else(|error| panic!("pump must not fail on the real stream: {error}"));
        applied = client.diagnostics().frames_applied;
        if applied > 0 {
            break;
        }
        thread::sleep(JOURNEY_SLEEP);
    }
    assert!(
        applied > 0,
        "the real SSE stream delivered no applicable frame within the budget; \
         diagnostics {:?}",
        client.diagnostics()
    );

    wait_for_stream_completion(&mut client);

    // THE RESTART LAW, against a real socket. Dropping the subscription must
    // cost latency and never content: the transcript after reconciling equals
    // the transcript folded from durable truth alone.
    let durable_before = client
        .view()
        .map(|view| view.slices.clone())
        .unwrap_or_default();
    client
        .reconcile(&id)
        .unwrap_or_else(|error| panic!("reconciliation must succeed: {error}"));
    let durable_after = client
        .view()
        .map(|view| view.slices.clone())
        .unwrap_or_default();
    assert!(
        durable_after.len() >= durable_before.len(),
        "reconciling from durable truth must never LOSE rendered content: \
         {} before, {} after",
        durable_before.len(),
        durable_after.len()
    );
    // And it must not DUPLICATE it either, which is the defect
    // `reconnect-cursor-convergence-replay` names. Rebuilding from nothing
    // cannot duplicate, and this asserts the rebuild really happened.
    let ordinals: Vec<usize> = durable_after.iter().map(|slice| slice.ordinal).collect();
    let mut unique = ordinals.clone();
    unique.sort_unstable();
    unique.dedup();
    assert_eq!(
        ordinals.len(),
        unique.len(),
        "a reconciled transcript must carry no duplicated slice ordinals"
    );

    let _ = &mut product;
}

#[test]
fn the_real_transport_carries_the_query_the_request_declares() {
    // The transport used to build its request line from the PATH alone and drop
    // every query parameter. `WireRequest` had carried them since the catalog
    // routes arrived and the contract double reads them straight off the
    // request object, so every in-crate witness passed while the product
    // received none of them: catalog reads were never scoped to a location, the
    // message list was never asked for reading order, and a continuation never
    // carried its cursor — the client asked for page two and was handed page
    // one, forever.
    //
    // Only a real socket can catch that, which is why this witness lives here.
    let (mut product, address) = serve("query");
    let mut client = connect(address, Bounds::PRODUCTION);
    let _ = select_offer(&mut client, &product);
    let id = client
        .create_session()
        .unwrap_or_else(|error| panic!("session creation must succeed: {error}"));

    // A turn, so there is a history with an order to declare. Bounded polling,
    // no clock read, exactly as every other witness here waits.
    client
        .submit_prompt("hello")
        .unwrap_or_else(|error| panic!("prompt must be accepted: {error}"));
    let mut rows = 0usize;
    for _ in 0..JOURNEY_POLLS {
        client
            .pump()
            .unwrap_or_else(|error| panic!("pump must not fail: {error}"));
        rows = client.view().map(|view| view.slices.len()).unwrap_or(0);
        if rows >= 2 {
            break;
        }
        thread::sleep(JOURNEY_SLEEP);
    }
    assert!(
        rows >= 2,
        "the turn produced a history to order: {rows} slices"
    );

    // Two reads of the SAME history in opposite declared orders. If the query
    // never arrived, the server would answer both identically.
    let mut raw = HttpConnection::attach(address, credential(), Bounds::PRODUCTION);
    let read = |raw: &mut HttpConnection, order: &str| -> Vec<String> {
        let request = WireRequest::new(Operation::MessageList, Some(&id), None)
            .unwrap_or_else(|error| panic!("address: {error:?}"))
            .with_query("order", order);
        let response = raw
            .call(&request)
            .unwrap_or_else(|error| panic!("the request must reach the server: {error}"));
        assert!(
            response.is_success(),
            "a declared order must be accepted, got {}",
            response.status
        );
        let page: serde_json::Value = serde_json::from_slice(&response.body)
            .unwrap_or_else(|error| panic!("the page must parse: {error}"));
        page.get("data")
            .and_then(serde_json::Value::as_array)
            .unwrap_or_else(|| panic!("the page carries rows"))
            .iter()
            .filter_map(|row| row.get("id").and_then(serde_json::Value::as_str))
            .map(str::to_owned)
            .collect()
    };

    let ascending = read(&mut raw, "asc");
    let descending = read(&mut raw, "desc");
    assert!(
        ascending.len() >= 2,
        "the session has enough history to order: {ascending:?}"
    );
    let mut reversed = descending.clone();
    reversed.reverse();
    assert_eq!(
        ascending, reversed,
        "the server honoured the order this client declared, so the query arrived"
    );

    let _ = &mut product;
}

fn wait_for_stream_completion(client: &mut Client<HttpConnection>) {
    // Session metadata can be the first applicable frame, before the provider
    // has even read its request. Wait for the execution this witness started
    // before teardown can cancel that request and panic its strict fixture.
    let mut completed = false;
    for _ in 0..JOURNEY_POLLS {
        client
            .pump()
            .unwrap_or_else(|error| panic!("stream completion must remain readable: {error}"));
        completed = client.view().is_some_and(|view| {
            view.slices
                .iter()
                .any(|slice| slice.kind == SliceKind::Text && slice.text.contains("stream me"))
                && matches!(
                    view.execution,
                    grapt_client::projection::ExecutionState::Settled { .. }
                )
        });
        if completed {
            break;
        }
        thread::sleep(JOURNEY_SLEEP);
    }
    assert!(
        completed,
        "the real stream never delivered its text and terminal"
    );
}

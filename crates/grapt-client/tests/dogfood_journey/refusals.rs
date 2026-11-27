//! refusals witnesses, split from the carrier by subject.
//!
//! Moved unchanged: the ratchet bounds a FILE, and a witness that was edited
//! on its way out of one would be a different witness.

use super::*;

#[test]
fn the_real_product_refuses_a_model_the_frozen_reference_does_not_carry() {
    // The failure counterexample for the model reference, proved against the
    // real server rather than asserted in a comment.
    //
    // `SessionModelSelect` takes a `Model.Ref` — id AND provider. This sends a
    // well-formed reference whose PROVIDER is wrong for the model, which is
    // exactly the shape a client that dropped the provider would produce once
    // two providers published the same model name. The real product must
    // refuse it; if it did not, the client's own provider-matching would be
    // unfalsifiable.
    let (mut product, address) = serve("model-ref");
    let mut client = connect(address, Bounds::PRODUCTION);
    let _ = select_offer(&mut client, &product);
    let id = client
        .create_session()
        .unwrap_or_else(|error| panic!("session creation must succeed: {error}"));

    let model = client
        .models()
        .iter()
        .find(|row| row.enabled)
        .cloned()
        .unwrap_or_else(|| panic!("an enabled model exists"));
    let Some(bogus) = grapt_client::wire::provider_id("no-such-provider").ok() else {
        panic!("the fixture provider id must parse")
    };
    let body = grapt_client::wire::encode_model_select(&model.id, &bogus)
        .unwrap_or_else(|error| panic!("the request must still encode: {error}"));

    let mut raw = HttpConnection::attach(address, credential(), Bounds::PRODUCTION);
    let request = WireRequest::new(Operation::SessionModelSelect, Some(&id), None)
        .unwrap_or_else(|error| panic!("address: {error:?}"))
        .with_body(body);
    let response = raw
        .call(&request)
        .unwrap_or_else(|error| panic!("the request must reach the server: {error}"));
    assert!(
        !response.is_success(),
        "a model reference naming an unknown provider must be refused, got {}",
        response.status
    );

    let _ = &mut product;
}

#[test]
fn a_wrong_credential_is_refused_by_the_real_server() {
    let (mut product, address) = serve("auth");

    // The POSITIVE control first. Without it, the refusal below could equally
    // be a server that refuses everything — an endpoint that was never really
    // serving, a route that does not exist, a port belonging to something else.
    let mut authorized = connect(address, Bounds::PRODUCTION);
    authorized
        .refresh_sessions()
        .unwrap_or_else(|error| panic!("the correct credential must be accepted: {error}"));

    let wrong = BasicCredential::new(USERNAME, "not-the-secret")
        .unwrap_or_else(|error| panic!("witness credential must construct: {error}"));
    let mut client = Client::new(
        HttpConnection::attach(address, wrong, Bounds::PRODUCTION),
        Bounds::PRODUCTION,
    );
    match client.refresh_sessions() {
        Err(ClientError::Status { status, .. }) => assert_eq!(
            status, 401,
            "a wrong secret must be refused as unauthorized, not merely fail"
        ),
        other => panic!("expected a 401 from the real server, got {other:?}"),
    }

    let _ = &mut product;
}

#[test]
fn an_oversized_real_response_is_refused_before_the_body_is_allocated() {
    // The response bound, exercised against a REAL server rather than a double
    // that was told to produce a large body. This is the only place the
    // PRE-ALLOCATION half can be observed end to end: the server declares a
    // `Content-Length`, and the transport refuses on the head alone.
    let (mut product, address) = serve("bound");

    // A bound small enough that a real session listing exceeds it, and non-zero
    // so it is a bound rather than a refusal-of-everything.
    let tiny = Bounds {
        max_response_bytes: 8,
        ..Bounds::PRODUCTION
    }
    .validate()
    .unwrap_or_else(|error| panic!("witness bounds: {error}"));

    // POSITIVE CONTROL: the same request under production bounds succeeds, so
    // the refusal below is the bound firing and not a broken endpoint.
    let mut generous = connect(address, Bounds::PRODUCTION);
    generous
        .refresh_sessions()
        .unwrap_or_else(|error| panic!("the unbounded listing must succeed: {error}"));

    let mut client = connect(address, tiny);
    match client.refresh_sessions() {
        Err(ClientError::Transport(TransportError::ResponseTooLarge { bound })) => {
            assert_eq!(bound, 8);
        }
        other => panic!("expected a pre-allocation refusal, got {other:?}"),
    }

    let _ = &mut product;
}

/// Without any catalog home the real product admits the prompt but does not
/// run it, and names why: the acceptance carries the queued reason and the
/// history a not-run-yet notice, never an answer.
/// The fixture strips HOME and every ambient catalog input from the child,
/// so this is the filing-namespace shape on any host. It proves the
/// per-root catalog home that every other witness receives is load-bearing.
#[test]
fn a_product_without_a_catalog_home_queues_the_prompt_by_name() {
    let (product, address) = serve_catalog("no-catalog-home", false);
    let mut client = connect(address, Bounds::PRODUCTION);
    let _ = select_offer(&mut client, &product);
    let id = client
        .create_session()
        .unwrap_or_else(|error| panic!("session creation must succeed: {error}"));
    let body = grapt_client::wire::encode_prompt("no catalog home")
        .unwrap_or_else(|error| panic!("the prompt must encode: {error}"));
    let mut raw = HttpConnection::attach(address, credential(), Bounds::PRODUCTION);
    let request = WireRequest::new(Operation::SessionPrompt, Some(&id), None)
        .unwrap_or_else(|error| panic!("prompt route: {error:?}"))
        .with_body(body);
    let response = raw
        .call(&request)
        .unwrap_or_else(|error| panic!("the prompt must reach the server: {error}"));
    let text = String::from_utf8_lossy(&response.body);
    assert_eq!(
        response.status, 200,
        "unexpected prompt status; body {text}"
    );
    let accepted: serde_json::Value = serde_json::from_slice(&response.body)
        .unwrap_or_else(|error| panic!("the acceptance must parse: {error}; body {text}"));
    assert!(
        accepted
            .pointer("/queued/reason")
            .and_then(serde_json::Value::as_str)
            .is_some_and(|reason| reason.contains("catalog home directory is not configured")),
        "the acceptance did not name the missing catalog home: {text}"
    );
    let request = WireRequest::new(Operation::MessageList, Some(&id), None)
        .unwrap_or_else(|error| panic!("address: {error:?}"));
    let response = raw
        .call(&request)
        .unwrap_or_else(|error| panic!("the history must reach the server: {error}"));
    let page: serde_json::Value = serde_json::from_slice(&response.body)
        .unwrap_or_else(|error| panic!("the history must parse: {error}"));
    let rows = page
        .get("data")
        .and_then(serde_json::Value::as_array)
        .unwrap_or_else(|| panic!("the history carries rows: {page}"));
    assert!(
        !rows.iter().any(|row| row["type"] == "assistant"),
        "the blocked prompt ran: {page}"
    );
    assert!(
        rows.iter().any(|row| row["metadata"]["notRunYet"] == true
            && row["text"]
                .as_str()
                .is_some_and(|text| text.contains("catalog home directory is not configured"))),
        "the history has no not-run-yet notice naming the missing catalog home: {page}"
    );
    drop(product);
}

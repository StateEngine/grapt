//! Terminal hydration stays local to the selected session and keeps SSE open.
use super::*;

pub(super) fn complete_usage(stream: &mut TcpStream, prompt: &str) {
    let events = [
        serde_json::json!({"type":"response.output_text.delta","delta":prompt}),
        serde_json::json!({"type":"response.output_text.done"}),
        serde_json::json!({"type":"response.completed","response":{"usage":{
            "input_tokens":11,"output_tokens":4,"total_tokens":15,
            "input_tokens_details":{"cached_tokens":2,"cache_write_tokens":1},
            "output_tokens_details":{"reasoning_tokens":0}
        }}}),
    ];
    let body = events
        .iter()
        .map(|event| format!("data: {event}\n\n"))
        .collect::<String>();
    write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len())
        .unwrap_or_else(|error| panic!("terminal provider failed: {error}"));
}

fn hydrate(client: &mut Client<HttpConnection>, before: usize, text: &str) {
    for _ in 0..JOURNEY_POLLS {
        client
            .pump()
            .unwrap_or_else(|error| panic!("terminal pump failed: {error}"));
        if client.diagnostics().reconciliations > before
            && client
                .view()
                .is_some_and(|view| view.slices.iter().any(|slice| slice.text == text))
        {
            return;
        }
        thread::sleep(JOURNEY_SLEEP);
    }
    panic!("terminal did not hydrate: {:?}", client.diagnostics());
}

#[test]
fn a_terminal_refreshes_only_its_session_without_reconnecting() {
    let (product, address) = serve_with_provider("terminal-local", true, complete_usage);
    let mut a = connect(address, Bounds::PRODUCTION);
    let mut b = connect(address, Bounds::PRODUCTION);
    let _ = select_offer(&mut a, &product);
    let _ = select_offer(&mut b, &product);
    let _a_id = a
        .create_session()
        .unwrap_or_else(|error| panic!("A session: {error}"));
    let _b_id = b
        .create_session()
        .unwrap_or_else(|error| panic!("B session: {error}"));
    a.peer_mut().resubscribe();
    b.peer_mut().resubscribe();
    prove_positions(&mut a);
    prove_positions(&mut b);
    let a_before = a.diagnostics().reconciliations;
    let b_before = b.diagnostics().reconciliations;
    a.submit_prompt("A finishes independently")
        .unwrap_or_else(|error| panic!("A prompt: {error}"));
    hydrate(&mut a, a_before, "A finishes independently");
    // Drain A's complete lifecycle from B's socket, including any EOF the
    // resurrection would have left behind. B's cursor must remain live.
    for _ in 0..10 {
        b.pump()
            .unwrap_or_else(|error| panic!("unrelated terminal pump: {error}"));
    }
    assert_eq!(
        b.diagnostics().reconciliations,
        b_before,
        "A reloaded B's history"
    );
    assert_eq!(b.diagnostics().stream_closes, 0, "A closed B's stream");
    assert_eq!(b.diagnostics().gaps_observed, 0, "A caused B NoBaseline");
    assert!(b.stream_state().is_live());
    b.submit_prompt("B continues on the same stream")
        .unwrap_or_else(|error| panic!("B prompt: {error}"));
    hydrate(&mut b, b_before, "B continues on the same stream");
    for _ in 0..3 {
        b.pump()
            .unwrap_or_else(|error| panic!("settled pump: {error}"));
    }
    assert_eq!(
        b.diagnostics().reconciliations,
        b_before + 1,
        "terminal refreshed more than once"
    );
    assert_eq!(b.diagnostics().stream_closes, 0, "own terminal reconnected");
    let view = b.view().unwrap_or_else(|| panic!("B view missing"));
    assert_eq!(
        view.usage.input,
        Some(8),
        "durable uncached input (11 minus 2 cached and 1 cache-write) was not hydrated"
    );
    assert_eq!(view.usage.output, Some(4));
    assert_eq!(
        view.slices
            .iter()
            .filter(|slice| slice.kind == SliceKind::Text
                && slice.text == "B continues on the same stream")
            .count(),
        1,
        "duplicate reply"
    );
}

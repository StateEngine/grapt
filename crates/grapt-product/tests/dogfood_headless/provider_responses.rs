//! HTTP and provider-event response writers for the loopback witnesses.
#[cfg(not(windows))]
use super::PollDeadline;
use super::OUTPUT_OBJECTIVE;
use std::io::Write;
use std::net::TcpStream;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread;
use std::time::Duration;

pub(crate) fn respond(stream: &mut TcpStream, status: u16, body: &str) {
    let reason = if status == 200 { "OK" } else { "Unauthorized" };
    let response = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    stream
        .write_all(response.as_bytes())
        .unwrap_or_else(|error| panic!("provider response failed: {error}"));
}

pub(crate) fn respond_with_headers(
    stream: &mut TcpStream,
    headers: &[(&'static str, String)],
    body: &str,
) {
    let extra: String = headers
        .iter()
        .map(|(name, value)| format!("{name}: {value}\r\n"))
        .collect();
    let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\n{extra}Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    stream
        .write_all(response.as_bytes())
        .unwrap_or_else(|error| panic!("provider response failed: {error}"));
}

pub(crate) fn respond_success_text(stream: &mut TcpStream, text: &str) {
    let body = format!(
        "data: {{\"type\":\"response.output_text.delta\",\"delta\":{}}}\n\n\
         data: {{\"type\":\"response.output_text.done\"}}\n\n\
         data: {{\"type\":\"response.completed\",\"response\":{{\"usage\":{{\"input_tokens\":11,\"output_tokens\":4,\"total_tokens\":15}}}}}}\n\n",
        serde_json::to_string(text)
            .unwrap_or_else(|error| panic!("provider success text encoding failed: {error}"))
    );
    respond(stream, 200, &body);
}

pub(crate) fn respond_output(stream: &mut TcpStream, bytes: usize) {
    const DELTA_BYTES: usize = 16 * 1_024;
    stream
        .write_all(
            b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n",
        )
        .unwrap_or_else(|error| panic!("provider chunked response head failed: {error}"));
    let mut remaining = bytes;
    let mut opening = (bytes > OUTPUT_OBJECTIVE.len()).then_some(OUTPUT_OBJECTIVE);
    while remaining > 0 {
        let lead = opening.take().unwrap_or_default();
        let count = remaining.min(DELTA_BYTES).max(lead.len() + 1);
        let frame = format!(
            "data: {{\"type\":\"response.output_text.delta\",\"delta\":{}}}\n\n",
            serde_json::Value::String(format!("{lead}{}", "x".repeat(count - lead.len())))
        );
        write_chunk(stream, frame.as_bytes());
        remaining -= count;
    }
    write_chunk(
        stream,
        b"data: {\"type\":\"response.output_text.done\"}\n\n",
    );
    write_chunk(
        stream,
        b"data: {\"type\":\"response.completed\",\"response\":{\"usage\":{\"input_tokens\":1,\"output_tokens\":1,\"total_tokens\":2,\"input_tokens_details\":{\"cached_tokens\":0,\"cache_write_tokens\":0},\"output_tokens_details\":{\"reasoning_tokens\":0}}}}\n\n",
    );
    end_after_terminal(stream, "provider chunked response terminal failed");
}

/// End chunked framing after a complete `response.completed` frame. A reader
/// may close as soon as it sees that terminal, so its disconnect while the
/// trailer is written (a broken pipe, or a reset when a later byte such as the
/// chunk's CRLF was still unread) is the consumer finishing, not a lost
/// response. Any other failure still fails the fixture; earlier writes keep
/// their own panics.
pub(crate) fn end_after_terminal(stream: &mut TcpStream, what: &str) {
    if let Err(error) = stream.write_all(b"0\r\n\r\n") {
        if !matches!(
            error.kind(),
            std::io::ErrorKind::BrokenPipe | std::io::ErrorKind::ConnectionReset
        ) {
            panic!("{what}: {error}");
        }
    }
}

pub(super) fn respond_tool_input(
    stream: &mut TcpStream,
    call: &str,
    tool: &str,
    arguments: &serde_json::Value,
) {
    let events = [
        serde_json::json!({"type":"response.output_item.added","item":{"type":"function_call","id":format!("item_{call}"),"call_id":call,"name":tool}}),
        serde_json::json!({"type":"response.function_call_arguments.delta","item_id":format!("item_{call}"),"delta":arguments.to_string()}),
        serde_json::json!({"type":"response.function_call_arguments.done","item_id":format!("item_{call}")}),
        serde_json::json!({"type":"response.completed","response":{"usage":{"input_tokens":1,"output_tokens":1,"total_tokens":2}}}),
    ];
    let body = events
        .iter()
        .map(|event| format!("data: {event}\n\n"))
        .collect::<String>();
    respond(stream, 200, &body);
}

pub(crate) fn respond_tool_read(stream: &mut TcpStream, call: &str, path: &str) {
    let arguments = serde_json::json!({"path": path}).to_string();
    let body = format!(
        "data: {{\"type\":\"response.output_item.added\",\"item\":{{\"type\":\"function_call\",\"id\":\"item_{call}\",\"call_id\":\"{call}\",\"name\":\"read\"}}}}\n\n\
         data: {{\"type\":\"response.function_call_arguments.delta\",\"item_id\":\"item_{call}\",\"delta\":{}}}\n\n\
         data: {{\"type\":\"response.function_call_arguments.done\",\"item_id\":\"item_{call}\"}}\n\n\
         data: {{\"type\":\"response.completed\",\"response\":{{\"usage\":{{\"input_tokens\":1,\"output_tokens\":1,\"total_tokens\":2}}}}}}\n\n",
        serde_json::to_string(&arguments)
            .unwrap_or_else(|error| panic!("tool arguments encoding failed: {error}"))
    );
    respond(stream, 200, &body);
}

pub(super) fn respond_tool_reads_measured(
    stream: &mut TcpStream,
    calls: &[(&str, &str)],
    input: usize,
) {
    let mut body = String::new();
    for (call, path) in calls {
        let arguments = serde_json::json!({"path": path}).to_string();
        body.push_str(&format!(
            "data: {{\"type\":\"response.output_item.added\",\"item\":{{\"type\":\"function_call\",\"id\":\"item_{call}\",\"call_id\":\"{call}\",\"name\":\"read\"}}}}\n\n\
             data: {{\"type\":\"response.function_call_arguments.delta\",\"item_id\":\"item_{call}\",\"delta\":{}}}\n\n\
             data: {{\"type\":\"response.function_call_arguments.done\",\"item_id\":\"item_{call}\"}}\n\n",
            serde_json::to_string(&arguments)
                .unwrap_or_else(|error| panic!("tool arguments encoding failed: {error}"))
        ));
    }
    body.push_str(&format!(
        "data: {{\"type\":\"response.completed\",\"response\":{{\"usage\":{{\"input_tokens\":{input},\"output_tokens\":1,\"total_tokens\":{}}}}}}}\n\n",
        input + 1
    ));
    respond(stream, 200, &body);
}

pub(super) fn respond_text_then_reads(
    stream: &mut TcpStream,
    text_bytes: usize,
    calls: &[(&str, &str)],
) {
    let text = "Observed work. ".repeat(text_bytes.div_ceil(15));
    let mut body = format!(
        "data: {{\"type\":\"response.output_text.delta\",\"delta\":{}}}\n\n\
         data: {{\"type\":\"response.output_text.done\"}}\n\n",
        serde_json::to_string(&text)
            .unwrap_or_else(|error| panic!("provider text encoding failed: {error}"))
    );
    tool_read_items(&mut body, calls);
    body.push_str("data: {\"type\":\"response.completed\",\"response\":{}}\n\n");
    respond(stream, 200, &body);
}

pub(super) fn respond_tool_reads_unmetered(stream: &mut TcpStream, calls: &[(&str, &str)]) {
    let mut body = String::new();
    tool_read_items(&mut body, calls);
    body.push_str("data: {\"type\":\"response.completed\",\"response\":{}}\n\n");
    respond(stream, 200, &body);
}

fn tool_read_items(body: &mut String, calls: &[(&str, &str)]) {
    for (call, path) in calls {
        let arguments = serde_json::json!({"path": path}).to_string();
        body.push_str(&format!(
            "data: {{\"type\":\"response.output_item.added\",\"item\":{{\"type\":\"function_call\",\"id\":\"item_{call}\",\"call_id\":\"{call}\",\"name\":\"read\"}}}}\n\n\
             data: {{\"type\":\"response.function_call_arguments.delta\",\"item_id\":\"item_{call}\",\"delta\":{}}}\n\n\
             data: {{\"type\":\"response.function_call_arguments.done\",\"item_id\":\"item_{call}\"}}\n\n",
            serde_json::to_string(&arguments)
                .unwrap_or_else(|error| panic!("tool arguments encoding failed: {error}"))
        ));
    }
}

pub(crate) fn respond_tool_reads(stream: &mut TcpStream, calls: &[(&str, &str)]) {
    let mut body = String::new();
    tool_read_items(&mut body, calls);
    body.push_str(
        "data: {\"type\":\"response.completed\",\"response\":{\"usage\":{\"input_tokens\":1,\"output_tokens\":1,\"total_tokens\":2}}}\n\n",
    );
    respond(stream, 200, &body);
}

pub(crate) fn respond_tool_plugin(stream: &mut TcpStream, call: &str, text: &str) {
    let arguments = serde_json::json!({"text": text}).to_string();
    let body = format!(
        "data: {{\"type\":\"response.output_item.added\",\"item\":{{\"type\":\"function_call\",\"id\":\"item_{call}\",\"call_id\":\"{call}\",\"name\":\"fixture_echo\"}}}}\n\n\
         data: {{\"type\":\"response.function_call_arguments.delta\",\"item_id\":\"item_{call}\",\"delta\":{}}}\n\n\
         data: {{\"type\":\"response.function_call_arguments.done\",\"item_id\":\"item_{call}\"}}\n\n\
         data: {{\"type\":\"response.completed\",\"response\":{{\"usage\":{{\"input_tokens\":1,\"output_tokens\":1,\"total_tokens\":2}}}}}}\n\n",
        serde_json::to_string(&arguments)
            .unwrap_or_else(|error| panic!("plugin arguments encoding failed: {error}"))
    );
    respond(stream, 200, &body);
}

#[cfg(not(windows))]
pub(crate) fn respond_tool_shell(stream: &mut TcpStream, call: &str, command: &str) {
    let arguments = serde_json::json!({"command": command, "timeout_ms": 20_000}).to_string();
    let body = format!(
        "data: {{\"type\":\"response.output_item.added\",\"item\":{{\"type\":\"function_call\",\"id\":\"item_{call}\",\"call_id\":\"{call}\",\"name\":\"shell\"}}}}\n\n\
         data: {{\"type\":\"response.function_call_arguments.delta\",\"item_id\":\"item_{call}\",\"delta\":{}}}\n\n\
         data: {{\"type\":\"response.function_call_arguments.done\",\"item_id\":\"item_{call}\"}}\n\n\
         data: {{\"type\":\"response.completed\",\"response\":{{\"usage\":{{\"input_tokens\":1,\"output_tokens\":1,\"total_tokens\":2}}}}}}\n\n",
        serde_json::to_string(&arguments)
            .unwrap_or_else(|error| panic!("shell arguments encoding failed: {error}"))
    );
    respond(stream, 200, &body);
}

pub(crate) fn respond_tool_history(stream: &mut TcpStream, call: &str, target: &str) {
    respond_tool_history_input(
        stream,
        call,
        &serde_json::json!({"call": target, "offset": 0}),
    );
}

pub(super) fn respond_tool_history_input(
    stream: &mut TcpStream,
    call: &str,
    arguments: &serde_json::Value,
) {
    let arguments = arguments.to_string();
    let body = format!(
        "data: {{\"type\":\"response.output_item.added\",\"item\":{{\"type\":\"function_call\",\"id\":\"item_{call}\",\"call_id\":\"{call}\",\"name\":\"history\"}}}}\n\n\
         data: {{\"type\":\"response.function_call_arguments.delta\",\"item_id\":\"item_{call}\",\"delta\":{}}}\n\n\
         data: {{\"type\":\"response.function_call_arguments.done\",\"item_id\":\"item_{call}\"}}\n\n\
         data: {{\"type\":\"response.completed\",\"response\":{{\"usage\":{{\"input_tokens\":1,\"output_tokens\":1,\"total_tokens\":2}}}}}}\n\n",
        serde_json::to_string(&arguments)
            .unwrap_or_else(|error| panic!("history arguments encoding failed: {error}"))
    );
    respond(stream, 200, &body);
}

pub(crate) fn respond_tool_subagent(stream: &mut TcpStream, call: &str, task: &str) {
    let arguments = serde_json::json!({ "task": task }).to_string();
    let body = format!(
        "data: {{\"type\":\"response.output_item.added\",\"item\":{{\"type\":\"function_call\",\"id\":\"item_{call}\",\"call_id\":\"{call}\",\"name\":\"subagent\"}}}}\n\n\
         data: {{\"type\":\"response.function_call_arguments.delta\",\"item_id\":\"item_{call}\",\"delta\":{}}}\n\n\
         data: {{\"type\":\"response.function_call_arguments.done\",\"item_id\":\"item_{call}\"}}\n\n\
         data: {{\"type\":\"response.completed\",\"response\":{{\"usage\":{{\"input_tokens\":1,\"output_tokens\":1,\"total_tokens\":2}}}}}}\n\n",
        serde_json::to_string(&arguments)
            .unwrap_or_else(|error| panic!("tool arguments encoding failed: {error}"))
    );
    respond(stream, 200, &body);
}

pub(crate) fn write_chunk(stream: &mut TcpStream, bytes: &[u8]) {
    let head = format!("{:x}\r\n", bytes.len());
    stream
        .write_all(head.as_bytes())
        .and_then(|()| stream.write_all(bytes))
        .and_then(|()| stream.write_all(b"\r\n"))
        .and_then(|()| stream.flush())
        .unwrap_or_else(|error| panic!("provider response chunk failed: {error}"));
}

pub(crate) fn stream_paced(
    stream: &mut TcpStream,
    label: &str,
    (burst, deltas, interval): (usize, usize, Duration),
    ticks: &AtomicUsize,
    (hold, breaks): (Option<&AtomicBool>, bool),
) {
    let send = |stream: &mut TcpStream, bytes: &[u8]| {
        let head = format!("{:x}\r\n", bytes.len());
        stream
            .write_all(head.as_bytes())
            .and_then(|()| stream.write_all(bytes))
            .and_then(|()| stream.write_all(b"\r\n"))
            .and_then(|()| stream.flush())
    };
    if stream
        .write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n")
        .is_err()
    {
        return;
    }
    let end = if breaks { "\\n" } else { " " };
    for sent in 0..burst + deltas {
        let tick = ticks.fetch_add(1, Ordering::SeqCst);
        let event = format!(
            "data: {{\"type\":\"response.output_text.delta\",\"delta\":\"{label}-{tick:06}{end}\"}}\n\n"
        );
        if send(stream, event.as_bytes()).is_err() {
            return;
        }
        if sent + 1 == burst {
            // Held after the burst until released, so whatever the test does
            // next happens while this stream still has deltas to send.
            for _ in 0..3_000 {
                if !hold.is_some_and(|hold| hold.load(Ordering::SeqCst)) {
                    break;
                }
                thread::sleep(Duration::from_millis(10));
            }
        }
        if sent >= burst {
            thread::sleep(interval);
        }
    }
    let finish = concat!(
        "data: {\"type\":\"response.output_text.done\"}\n\n",
        "data: {\"type\":\"response.completed\",\"response\":{\"usage\":{\"input_tokens\":11,\"output_tokens\":4,\"total_tokens\":15}}}\n\n"
    );
    let _ = send(stream, finish.as_bytes()).and_then(|()| stream.write_all(b"0\r\n\r\n"));
}

pub(crate) fn stall_before_first_fragment(_stream: &mut TcpStream) {
    thread::sleep(Duration::from_secs(2));
}

pub(crate) fn stream_text_then_stall(stream: &mut TcpStream) {
    stream
        .write_all(
            b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n",
        )
        .unwrap_or_else(|error| panic!("streaming provider head failed: {error}"));
    write_chunk(
        stream,
        b"data: {\"type\":\"response.output_text.delta\",\"delta\":\"Streamed before \"}\n\n",
    );
    // Past the durable owner's one-second checkpoint interval.
    thread::sleep(Duration::from_millis(1_200));
    write_chunk(
        stream,
        b"data: {\"type\":\"response.output_text.delta\",\"delta\":\"the crash.\"}\n\n",
    );
    // Hold until the killed server's end of the connection closes.
    let _ = stream.set_read_timeout(Some(Duration::from_secs(20)));
    let _ = std::io::Read::read(stream, &mut [0u8; 64]);
}

#[cfg(not(windows))]
pub(super) fn respond_partial_then_held_disconnect(
    stream: &mut TcpStream,
    entered: &AtomicBool,
    release: &AtomicBool,
) {
    let body = b"data: {\"type\":\"response.output_text.delta\",\"delta\":\"visible partial\"}\n\n";
    stream
        .write_all(
            b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n",
        )
        .unwrap_or_else(|error| panic!("partial provider response head failed: {error}"));
    write_chunk(stream, body);
    entered.store(true, Ordering::SeqCst);
    let wait = PollDeadline::within(Duration::from_secs(10));
    while wait.pending() {
        if release.load(Ordering::SeqCst) {
            return;
        }
        thread::sleep(Duration::from_millis(10));
    }
}

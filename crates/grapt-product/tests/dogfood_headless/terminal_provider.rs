//! Terminal response fixtures for process I/O and ACP concurrency journeys.
use super::*;

pub(super) fn respond_phased_output(
    stream: &mut TcpStream,
    advance: &terminal_coordination::Phase,
    finish: &terminal_coordination::Phase,
    stop: &AtomicBool,
) -> Result<(), String> {
    stream
        .set_write_timeout(Some(Duration::from_secs(2)))
        .map_err(|error| format!("SSE phased provider write deadline: {error}"))?;
    stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n")
        .unwrap_or_else(|error| panic!("phased provider head: {error}"));
    let write = |stream: &mut TcpStream, event: serde_json::Value| {
        let frame = format!("data: {event}\n\n");
        write!(stream, "{:x}\r\n{frame}\r\n", frame.len())
            .unwrap_or_else(|error| panic!("phased provider frame: {error}"));
    };
    write(
        stream,
        serde_json::json!({"type":"response.output_text.delta","delta":"before A; "}),
    );
    advance.wait("advance", stop)?;
    write(
        stream,
        serde_json::json!({"type":"response.output_text.delta","delta":"after A; "}),
    );
    finish.wait("finish", stop)?;
    write(
        stream,
        serde_json::json!({"type":"response.output_text.done"}),
    );
    write(
        stream,
        serde_json::json!({"type":"response.completed","response":{"usage":{
            "input_tokens":1,"output_tokens":1,"total_tokens":2,
            "input_tokens_details":{"cached_tokens":0,"cache_write_tokens":0},
            "output_tokens_details":{"reasoning_tokens":0}
        }}}),
    );
    responses::end_after_terminal(stream, "phased provider end");
    Ok(())
}

#[cfg(unix)]
pub(super) fn operation_from_body(body: &str, call: &str) -> Result<String, String> {
    let body: serde_json::Value = serde_json::from_str(body).map_err(|error| error.to_string())?;
    let output = body["input"]
        .as_array()
        .ok_or("provider input absent")?
        .iter()
        .find(|item| item["type"] == "function_call_output" && item["call_id"] == call)
        .and_then(|item| item["output"].as_str())
        .ok_or("terminal start output absent")?;
    let output: serde_json::Value =
        serde_json::from_str(output).map_err(|error| error.to_string())?;
    output["session_id"]
        .as_str()
        .map(str::to_owned)
        .ok_or_else(|| "terminal start identity absent".into())
}

/// A tool may begin while its parent is streaming. Its child waits for the
/// provider terminal, then runs while the parent's HTTP tail is still open.
pub(super) fn respond_phased_subagent(
    stream: &mut TcpStream,
    call: &str,
    task: &str,
    finish: &terminal_coordination::Phase,
    tail_release: &terminal_coordination::Phase,
    stop: &AtomicBool,
) -> Result<(), String> {
    stream
        .set_write_timeout(Some(Duration::from_secs(2)))
        .map_err(|error| format!("subagent provider write deadline: {error}"))?;
    stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n")
        .map_err(|error| format!("subagent provider head: {error}"))?;
    let item = format!("item_{call}");
    for event in [
        serde_json::json!({"type":"response.output_item.added","item":{"type":"function_call","id":item,"call_id":call,"name":"subagent"}}),
        serde_json::json!({"type":"response.function_call_arguments.delta","item_id":item,"delta":serde_json::json!({"task":task}).to_string()}),
        serde_json::json!({"type":"response.function_call_arguments.done","item_id":item}),
    ] {
        responses::write_chunk(stream, format!("data: {event}\n\n").as_bytes());
    }
    finish.wait("parent provider terminal", stop)?;
    responses::write_chunk(stream, b"data: {\"type\":\"response.completed\",\"response\":{\"usage\":{\"input_tokens\":1,\"output_tokens\":1,\"total_tokens\":2}}}\n\n");
    // Deliberately do not terminate HTTP framing until the complete logical
    // parent (including its child tool result and continuation) has finished.
    tail_release.wait("parent HTTP tail after child completion", stop)
}

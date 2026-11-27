//! SC-API-01: real tool output can fill a history page before its row limit.
use super::*;
use std::collections::BTreeSet;

const GROUPS: usize = 1;
const SEEDED_OUTPUTS: usize = 45;

#[path = "message_page_bytes/seed.rs"]
mod seed;
#[path = "message_page_bytes/synthetic.rs"]
mod synthetic;
const CALLS_PER_GROUP: usize = 5;

fn reads(group: usize) -> Script {
    let mut body = String::new();
    for index in 0..CALLS_PER_GROUP {
        let call = format!("call_api01_{group}_{index}");
        let arguments = serde_json::json!({"path":"large.txt","line_count":2000}).to_string();
        for frame in [
            serde_json::json!({"type":"response.output_item.added","item":{
                "type":"function_call","id":format!("item_{call}"),"call_id":call,"name":"read_lines"}}),
            serde_json::json!({"type":"response.function_call_arguments.delta",
                "item_id":format!("item_{call}"),"delta":arguments}),
            serde_json::json!({"type":"response.function_call_arguments.done","item_id":format!("item_{call}")}),
        ] {
            body.push_str(&format!("data: {frame}\n\n"));
        }
    }
    body.push_str("data: {\"type\":\"response.completed\",\"response\":{\"usage\":{\"input_tokens\":1,\"output_tokens\":1,\"total_tokens\":2}}}\n\n");
    Script::Raw(body)
}

fn collect(address: SocketAddr, session: &str, limit: usize) -> Vec<serde_json::Value> {
    let mut path = format!("/api/session/{session}/message?order=desc&limit={limit}");
    let mut rows = Vec::new();
    let mut seen = BTreeSet::new();
    for _ in 0..100 {
        let (status, bytes) =
            request_with_read_timeout(address, "GET", &path, "", Duration::from_secs(30));
        assert_eq!(
            status,
            200,
            "API01_PAGE_BYTES: status={status}, body_bytes={}",
            bytes.len()
        );
        assert!(bytes.len() <= grapt_api::u3a::U3A_PRODUCTION_LIMITS.page_bytes);
        let page: serde_json::Value = serde_json::from_slice(&bytes)
            .unwrap_or_else(|error| panic!("history page JSON: {error}"));
        let data = page["data"]
            .as_array()
            .unwrap_or_else(|| panic!("page data absent"));
        if data.is_empty() {
            return rows;
        }
        for row in data {
            let id = row["id"]
                .as_str()
                .unwrap_or_else(|| panic!("row id absent"));
            assert!(seen.insert(id.to_owned()), "cursor repeated row {id}");
            rows.push(row.clone());
        }
        let Some(cursor) = page["cursor"]["next"].as_str() else {
            return rows;
        };
        path = format!("/api/session/{session}/message?limit={limit}&cursor={cursor}");
    }
    panic!("history cursor did not terminate within 100 pages");
}

#[test]
fn api01_large_tool_history_pages_by_encoded_bytes_without_losing_rows(
) -> Result<(), Box<dyn std::error::Error>> {
    let _journey = process_journey_custody();
    let root = root("message-page-bytes");
    let _cleanup = terminal_coordination::RootCleanup(root.clone());
    prepare(&root, 240_000);
    prepare_model_inventory(&root, 1_000_000);
    set_compaction(&root, serde_json::json!({"auto":false}));
    // Short enough lines to avoid display cuts, large enough to exceed 8 MiB
    // across 45 ordinary outputs; each remains below the 256 KiB DTO bound.
    let line = format!("API01_OUTPUT{}\n", "x".repeat(980));
    fs::write(root.join("large.txt"), line.repeat(180))?;
    let scripts = vec![reads(0), Script::SuccessText("Read the requested files.")];
    let provider = ProviderFixture::start(scripts);
    let mut child = spawn(&root, provider.address);
    let address = await_ready(&root, &mut child);
    let _approver = approval::Attachment::open(address, "*");
    let session = create(address);
    json(
        address,
        "POST",
        &format!("/api/session/{session}/prompt"),
        r#"{"id":"msg_api01_seed","text":"Read the five files.","delivery":"queue","resume":true}"#,
    );
    manual_compaction::assert_carried(&root, "msg_api01_seed", "session.execution.succeeded.1");
    permission_residuals::wait_session_idle(address, &session);
    assert_eq!(provider.calls.load(Ordering::SeqCst), 2);
    child.stop_after_witness(&root);
    drop(child);
    // Extend the stopped disposable Session through the runtime's ordinary
    // durable writer. Reopening the real process exercises store replay,
    // artifact reads, DTO encoding and the authenticated HTTP route without
    // depending on compaction to construct a response-size fixture.
    seed::outputs(&root, &session, SEEDED_OUTPUTS)?;
    let mut child = spawn(&root, provider.address);
    let address = await_ready(&root, &mut child);
    let first_path = format!("/api/session/{session}/message?order=desc&limit=50");
    let (status, body) =
        request_with_read_timeout(address, "GET", &first_path, "", Duration::from_secs(30));
    eprintln!(
        "API01_PAGE_BYTES first response: status={status} body_bytes={}",
        body.len()
    );
    assert_eq!(
        status,
        200,
        "API01_PAGE_BYTES: TUI page returned status={status}, body_bytes={}",
        body.len()
    );
    let first: serde_json::Value = serde_json::from_slice(&body)?;
    let reference = collect(address, &session, 1);
    let rows = collect(address, &session, 50);
    let wide = collect(address, &session, 200);
    let ids =
        |rows: &[serde_json::Value]| rows.iter().map(|row| row["id"].clone()).collect::<Vec<_>>();
    assert_eq!(ids(&rows), ids(&reference));
    assert_eq!(ids(&wide), ids(&reference));
    assert!(first["data"].as_array().ok_or("data")?.len() < reference.len());
    assert!(first["cursor"]["next"].is_string());
    let outputs = rows
        .iter()
        .filter_map(|row| row["content"].as_array())
        .flatten()
        .filter(|part| part["type"] == "tool" && part["state"].to_string().contains("API01_OUTPUT"))
        .count();
    assert_eq!(outputs, GROUPS * CALLS_PER_GROUP + SEEDED_OUTPUTS);
    child.stop_after_witness(&root);
    drop(child);
    let oversized = seed::oversized_row(&root, &session)?;
    let mut child = spawn(&root, provider.address);
    let address = await_ready(&root, &mut child);
    for path in [
        format!("/api/session/{session}/message?order=desc&limit=50"),
        format!("/api/session/{session}/message/{oversized}"),
    ] {
        let (status, body) =
            request_with_read_timeout(address, "GET", &path, "", Duration::from_secs(30));
        assert_eq!(status, 500);
        let refusal: serde_json::Value = serde_json::from_slice(&body)?;
        assert_eq!(refusal["_tag"], "UnknownError");
        let message = refusal["message"].as_str().ok_or("refusal message")?;
        assert!(
            message.contains(&oversized) && message.contains("8388608"),
            "row refusal: {message}"
        );
    }
    child.stop_after_witness(&root);
    Ok(())
}

//! Real authenticated permission pages and live SSE, without paid providers.
use super::*;

#[path = "permission_preview/status_commit_failure.rs"]
mod status_commit_failure;

fn frames(text: &str) -> Vec<serde_json::Value> {
    // The target can arrive with the start of a later frame in the same TCP
    // read. SSE dispatches complete frames; the trailing fragment is pending.
    text.split_inclusive("\n\n")
        .filter(|frame| frame.ends_with("\n\n"))
        .filter_map(|frame| frame.lines().find_map(|line| line.strip_prefix("data: ")))
        .map(|data| serde_json::from_str(data).unwrap_or_else(|error| panic!("SSE JSON: {error}")))
        .collect()
}

#[test]
fn http_permission_creation_and_resolution_publish_the_committed_preview() {
    let _journey = process_journey_custody();
    let root = root("approval-preview-http");
    prepare(&root, 60_000);
    let provider = ProviderFixture::start(Vec::new());
    let mut child = process_fixture::spawn_controlled(&root, provider.address);
    let address = await_ready(&root, &mut child);
    let session = create(address);
    let _approver = approval::Attachment::open(address, &session);
    let (stream, buffer) = event_stream(address);
    let preview = serde_json::json!({"type":"shell","command":"pwd", "cwd":"/repo", "shell":"/bin/sh", "truncated":false});
    let body = serde_json::json!({"id":"per_preview_http", "agent":"agt_live_headless1", "action":"external_directory", "resources":["/repo/file"],
        "metadata":{"request":{"approvalPreview":preview}}});
    let path = format!("/api/session/{session}/permission");
    let asked = json(address, "POST", &path, &body.to_string());
    assert_eq!(asked["data"]["effect"], "ask");
    let page = json(address, "GET", &path, "");
    assert_eq!(page["data"][0]["preview"], preview);
    let (status, _) = request(
        address,
        "POST",
        &format!("{path}/per_preview_http/reply"),
        r#"{"reply":"once"}"#,
    );
    assert_eq!(status, 204);
    let events = frames(&collect(stream, buffer, "permission.replied"));
    let asked = events
        .iter()
        .find(|event| {
            event["type"] == "permission.asked" && event["data"]["id"] == "per_preview_http"
        })
        .unwrap_or_else(|| panic!("committed HTTP ask was not published"));
    let replied = events
        .iter()
        .find(|event| {
            event["type"] == "permission.replied"
                && event["data"]["requestID"] == "per_preview_http"
        })
        .unwrap_or_else(|| panic!("committed HTTP reply was not published"));
    assert_eq!(asked["data"]["preview"], preview);
    let asked_seq = asked["durable"]["seq"]
        .as_u64()
        .unwrap_or_else(|| panic!("ask sequence missing"));
    let replied_seq = replied["durable"]["seq"]
        .as_u64()
        .unwrap_or_else(|| panic!("reply sequence missing"));
    assert!(asked_seq < replied_seq);
    child.stop_after_witness(&root);
    drop(child);
    process_fixture::cleanup(&root);
}

#[test]
fn interrupted_patch_ask_publishes_cancellation_and_preserves_the_original_file() {
    patch_cancellation(false);
}

#[test]
fn approver_loss_publishes_cancellation_and_preserves_the_original_file() {
    patch_cancellation(true);
}

fn patch_cancellation(approver_loss: bool) {
    let _journey = process_journey_custody();
    let root = root(&format!("approval-preview-cancel-{approver_loss}"));
    prepare(&root, 60_000);
    attended(&root);
    // Author asks before overwriting existing bytes, so this fixture keeps an
    // actual pending proposal regardless of the default Maintainer policy.
    let target = root.join("preview.txt");
    fs::write(&target, b"original\n").unwrap_or_else(|error| panic!("preview fixture: {error}"));
    let provider = ProviderFixture::start(vec![
        Script::ToolInput {
            call: "call_preview_cancel",
            tool: "write",
            arguments: serde_json::json!({"path":"preview.txt","content":"replacement\n"}),
        },
        Script::Success,
    ]);
    let mut child = process_fixture::spawn_controlled(&root, provider.address);
    let address = await_ready(&root, &mut child);
    let session = create(address);
    let mut approver = Some(approval::Attachment::open(address, &session));
    let (stream, buffer) = event_stream(address);
    let prompt = json(
        address,
        "POST",
        &format!("/api/session/{session}/prompt"),
        r#"{"id":"msg_preview_cancel","text":"overwrite the proposed file","delivery":"queue","resume":true}"#,
    );
    assert!(prompt.to_string().contains("msg_preview_cancel"));
    let row = permission_boundaries::pending(address, &session, "call_preview_cancel");
    assert_eq!(row["preview"]["type"], "patch");
    assert_eq!(row["preview"]["format"], "write");
    let patch = row["preview"]["patch"]
        .as_str()
        .unwrap_or_else(|| panic!("patch preview missing"));
    assert!(patch.contains("+++ preview.txt"), "{patch}");
    assert!(patch.contains("+replacement\n"), "{patch}");
    assert_eq!(
        fs::read(&target).unwrap_or_else(|error| panic!("{error}")),
        b"original\n"
    );
    if approver_loss {
        drop(approver.take());
        assert!(
            permission_residuals::wait_until(
                client_allowance::CLI_REQUEST_ALLOWANCE,
                permission_residuals::SETTLE_POLL,
                || provider.calls.load(Ordering::SeqCst) == 2,
            )
            .is_some(),
            "approver loss stranded the pending patch"
        );
        assert!(
            permission_boundaries::output(&provider, "call_preview_cancel")
                .contains("ApprovalUnavailable")
        );
    } else {
        let (status, _) = request(
            address,
            "POST",
            &format!("/api/session/{session}/interrupt"),
            "{}",
        );
        assert_eq!(status, 204);
    }
    permission_residuals::wait_session_idle(address, &session);
    assert_eq!(
        provider.calls.load(Ordering::SeqCst),
        if approver_loss { 2 } else { 1 },
        "cancellation did not preserve the expected provider continuation"
    );
    let events = frames(&collect(stream, buffer, "permission.cancelled"));
    let cancelled = events
        .iter()
        .find(|event| event["type"] == "permission.cancelled")
        .unwrap_or_else(|| panic!("committed cancellation was not published"));
    assert_eq!(cancelled["data"]["requestID"], row["id"]);
    assert_eq!(
        cancelled["data"]["reason"],
        if approver_loss {
            "approval-unavailable"
        } else {
            "interrupted"
        }
    );
    assert_eq!(cancelled["durable"]["version"], 1);
    let page = json(
        address,
        "GET",
        &format!("/api/session/{session}/permission"),
        "",
    );
    assert_eq!(page["data"].as_array().map(Vec::len), Some(0));
    assert_eq!(
        fs::read(&target).unwrap_or_else(|error| panic!("{error}")),
        b"original\n",
        "cancellation executed the proposed overwrite"
    );
    child.stop_after_witness(&root);
    drop(child);
    process_fixture::cleanup(&root);
}

#[test]
fn oversized_patch_preview_truncates_with_a_marker_and_the_full_patch_applies() {
    let _journey = process_journey_custody();
    let root = root("approval-preview-oversized");
    prepare(&root, 60_000);
    attended(&root);
    // Author asks before overwriting existing bytes, so this fixture keeps an
    // actual pending proposal regardless of the default Maintainer policy.
    let target = root.join("big.txt");
    fs::write(&target, b"original\n").unwrap_or_else(|error| panic!("preview fixture: {error}"));
    let body = "x".repeat(20_000);
    let patch = format!("*** Begin Patch\n*** Add File: big.txt\n+{body}\n*** End Patch");
    let provider = ProviderFixture::start(vec![
        Script::ToolInput {
            call: "call_preview_oversized",
            tool: "apply_patch",
            arguments: serde_json::json!({"input": patch.clone()}),
        },
        Script::Success,
    ]);
    let mut child = process_fixture::spawn_controlled(&root, provider.address);
    let address = await_ready(&root, &mut child);
    let session = create(address);
    let _approver = approval::Attachment::open(address, &session);
    let prompt = json(
        address,
        "POST",
        &format!("/api/session/{session}/prompt"),
        r#"{"id":"msg_preview_oversized","text":"add the generated file","delivery":"queue","resume":true}"#,
    );
    assert!(prompt.to_string().contains("msg_preview_oversized"));
    let row = permission_boundaries::pending(address, &session, "call_preview_oversized");
    assert_eq!(row["preview"]["type"], "patch");
    assert_eq!(row["preview"]["truncated"], true);
    let shown = row["preview"]["patch"]
        .as_str()
        .unwrap_or_else(|| panic!("patch preview missing"));
    let (head, marker) = shown
        .rsplit_once('\n')
        .unwrap_or_else(|| panic!("truncation marker missing: {shown}"));
    assert!(marker.starts_with('\u{2026}'), "{marker}");
    assert!(marker.ends_with(" more bytes"), "{marker}");
    assert!(
        patch.starts_with(head),
        "visible preview is not a prefix of the proposed patch"
    );
    let omitted: usize = marker
        .trim_start_matches('\u{2026}')
        .trim_start()
        .split(' ')
        .next()
        .unwrap_or_else(|| panic!("marker bytes missing: {marker}"))
        .parse()
        .unwrap_or_else(|_| panic!("marker bytes not a number: {marker}"));
    assert_eq!(
        patch.len() - head.len(),
        omitted,
        "the marker must name the omitted bytes exactly"
    );
    write_overwrite::reply(address, &session, &row, "once");
    permission_residuals::wait_session_idle(address, &session);
    assert_eq!(
        fs::read_to_string(&target).unwrap_or_else(|error| panic!("{error}")),
        format!("{body}\n"),
        "the approved full patch did not apply"
    );
    child.stop_after_witness(&root);
    drop(child);
    process_fixture::cleanup(&root);
}

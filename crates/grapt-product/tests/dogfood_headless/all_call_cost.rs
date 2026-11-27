//! The ordinary HTTP/SSE path prices summaries and preserves retry coverage.
use super::*;

fn priced_profile(root: &Path) {
    prepare(root, 120_000);
    for name in ["grapt.jsonc", "model-catalog.json", "account-models.json"] {
        let path = root.join(name);
        let text = fs::read_to_string(&path).unwrap_or_else(|e| panic!("cost profile read: {e}"));
        assert!(text.contains("gpt-live"), "fixture model disappeared");
        fs::write(path, text.replace("gpt-live", "gpt-6.1-sol"))
            .unwrap_or_else(|e| panic!("cost profile write: {e}"));
    }
}
fn measured(text: &str, input: u64, read: u64) -> Script {
    let frames = [
        serde_json::json!({"type":"response.output_text.delta","delta":text}),
        serde_json::json!({"type":"response.output_text.done"}),
        serde_json::json!({"type":"response.completed","response":{"usage":{
            "input_tokens":input,"input_tokens_details":{"cached_tokens":read,"cache_write_tokens":0},
            "output_tokens":10,"output_tokens_details":{"reasoning_tokens":4},"total_tokens":input+10,
        }}}),
    ];
    Script::Raw(
        frames
            .iter()
            .map(|frame| format!("data: {frame}\n\n"))
            .collect(),
    )
}
fn prompt_cost(address: SocketAddr, session: &str, id: &str) {
    json(
        address,
        "POST",
        &format!("/api/session/{session}/prompt"),
        &serde_json::json!({"id":id,"text":"continue the task","delivery":"queue","resume":true})
            .to_string(),
    );
    permission_residuals::wait_session_idle(address, session);
}
fn history(address: SocketAddr, session: &str) -> serde_json::Value {
    json(
        address,
        "GET",
        &format!("/api/session/{session}/message?order=asc&limit=50&diagnostics=cache"),
        "",
    )
}
fn last_calls(page: &serde_json::Value) -> &serde_json::Value {
    &page["data"]
        .as_array()
        .unwrap_or_else(|| panic!("history rows absent"))
        .iter()
        .rev()
        .find(|row| row["type"] == "assistant")
        .unwrap_or_else(|| panic!("assistant absent"))["metadata"]["cacheObservedCalls"]
}
#[test]
fn all_call_cost_prices_coding_and_compaction_and_reopens_without_double_counting() {
    let _journey = process_journey_custody();
    let root = root("all-call-cost-summary");
    let _cleanup = terminal_coordination::RootCleanup(root.clone());
    priced_profile(&root);
    let provider = ProviderFixture::start(vec![
        measured("first answer", 1000, 900),
        measured("## Objective\n- continue the task", 2000, 1000),
        measured("answer after summary", 3000, 2900),
    ]);
    let mut child = spawn(&root, provider.address);
    let address = await_ready(&root, &mut child);
    let session = create(address);
    prompt_cost(address, &session, "msg_cost_before_summary");
    json(
        address,
        "POST",
        &format!("/api/session/{session}/compact"),
        "{}",
    );
    prompt_cost(address, &session, "msg_cost_after_summary");
    assert_eq!(provider.calls.load(Ordering::SeqCst), 3);
    let first = history(address, &session);
    let calls = last_calls(&first);
    assert_eq!(calls["attempts"], 4);
    assert_eq!(calls["cost"]["status"], "subtotal");
    assert_eq!(calls["cost"]["priced"], 3);
    assert_eq!(calls["cost"]["missingUsage"], 1);
    assert_eq!(calls["cost"]["missingPricing"], 0);
    // Fresh 1200 at $2/M + cached 4800 at $0.1/M + output 30 at $10/M.
    assert!(
        (calls["cost"]["usd"]
            .as_f64()
            .unwrap_or_else(|| panic!("cost absent"))
            - 0.00318)
            .abs()
            < 1e-12
    );
    assert_eq!(calls["cost"]["subscription"], "unknown");
    // A fresh history read is the same path a closed/reopened TUI uses.
    assert_eq!(history(address, &session), first);
    child.stop_after_witness(&root);
    drop(child);
    let mut child = spawn(&root, provider.address);
    let address = await_ready(&root, &mut child);
    let restarted = history(address, &session);
    assert!(
        last_calls(&restarted).is_null(),
        "a process-local capture cannot survive a service restart"
    );
    let assistants: Vec<_> = restarted["data"]
        .as_array()
        .unwrap_or_else(|| panic!("history absent"))
        .iter()
        .filter(|row| row["type"] == "assistant")
        .collect();
    assert_eq!(assistants.len(), 2);
    assert!(
        assistants
            .iter()
            .all(|row| row["metadata"]["estimatedCost"]["status"] == "estimated"),
        "existing durable assistant estimates must remain available"
    );
    child.stop_after_witness(&root);
}
#[test]
fn all_call_cost_retry_with_no_usage_is_a_subtotal_not_a_free_attempt() {
    let _journey = process_journey_custody();
    let root = root("all-call-cost-retry");
    let _cleanup = terminal_coordination::RootCleanup(root.clone());
    priced_profile(&root);
    let provider = ProviderFixture::start(vec![
        Script::Refused {
            status: 429,
            body: r#"{"error":{"type":"rate_limit_exceeded"}}"#,
        },
        measured("recovered", 1000, 900),
    ]);
    let mut child = spawn(&root, provider.address);
    let address = await_ready(&root, &mut child);
    let session = create(address);
    prompt_cost(address, &session, "msg_cost_retry");
    assert_eq!(provider.calls.load(Ordering::SeqCst), 2);
    let page = history(address, &session);
    let calls = last_calls(&page);
    assert_eq!(calls["attempts"], 2);
    assert_eq!(calls["cost"]["status"], "subtotal");
    assert_eq!(calls["cost"]["priced"], 1);
    assert_eq!(calls["cost"]["missingUsage"], 1);
    assert_eq!(calls["cost"]["missingPricing"], 0);
    assert!(
        (calls["cost"]["usd"]
            .as_f64()
            .unwrap_or_else(|| panic!("subtotal absent"))
            - 0.00039)
            .abs()
            < 1e-12
    );
    child.stop_after_witness(&root);
}
#[test]
fn all_call_cost_unlisted_model_retains_usage_with_explicit_missing_pricing() {
    let _journey = process_journey_custody();
    let root = root("all-call-cost-unpriced");
    let _cleanup = terminal_coordination::RootCleanup(root.clone());
    prepare(&root, 120_000);
    let provider = ProviderFixture::start(vec![measured("unpriced", 1000, 900)]);
    let mut child = spawn(&root, provider.address);
    let address = await_ready(&root, &mut child);
    let session = create(address);
    prompt_cost(address, &session, "msg_cost_unpriced");
    let page = history(address, &session);
    let calls = last_calls(&page);
    assert_eq!(calls["pairedInput"], 1000);
    assert_eq!(calls["pairedRead"], 900);
    assert_eq!(calls["cost"]["status"], "unavailable");
    assert_eq!(calls["cost"]["priced"], 0);
    assert_eq!(calls["cost"]["missingUsage"], 0);
    assert_eq!(calls["cost"]["missingPricing"], 1);
    assert!(calls["cost"]["usd"].is_null());
    child.stop_after_witness(&root);
}

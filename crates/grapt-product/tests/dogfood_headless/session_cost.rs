//! Catalog-derived API estimates must survive an ordinary service reopen.
use super::*;

#[test]
fn chatgpt_sol_usage_never_becomes_a_catalog_subscription_charge(
) -> Result<(), Box<dyn std::error::Error>> {
    let _journey = process_journey_custody();
    let root = root("session-cost-chatgpt");
    let _cleanup = terminal_coordination::RootCleanup(root.clone());
    prepare(&root, 120_000);
    for name in ["grapt.jsonc", "model-catalog.json", "account-models.json"] {
        let path = root.join(name);
        fs::write(
            &path,
            fs::read_to_string(&path)?.replace("gpt-live", "gpt-6.1-sol"),
        )?;
    }
    let path = root.join("grapt.jsonc");
    let mut config: serde_json::Value = serde_json::from_slice(&fs::read(&path)?)?;
    config["provider"]["protocol_family"] = serde_json::json!("chatgpt-codex");
    fs::write(path, serde_json::to_vec(&config)?)?;
    let path = root.join("model-catalog.json");
    let mut catalog: serde_json::Value = serde_json::from_slice(&fs::read(&path)?)?;
    catalog["openai"]["models"]["gpt-6.1-sol"]["cost"] =
        serde_json::json!({"input":2,"output":10,"cache_read":0.1,"cache_write":2.5});
    fs::write(path, serde_json::to_vec(&catalog)?)?;
    let frames = [
        serde_json::json!({"type":"response.output_text.delta","delta":"done"}),
        serde_json::json!({"type":"response.output_text.done"}),
        serde_json::json!({"type":"response.completed","response":{"usage":{"input_tokens":1000,
            "input_tokens_details":{"cached_tokens":900,"cache_write_tokens":0},"output_tokens":10,
            "output_tokens_details":{"reasoning_tokens":4},"total_tokens":1010}}}),
    ];
    let provider = ProviderFixture::start(vec![Script::Raw(
        frames
            .iter()
            .map(|frame| format!("data: {frame}\n\n"))
            .collect(),
    )]);
    let mut child = spawn(&root, provider.address);
    let address = await_ready(&root, &mut child);
    let session = create(address);
    prompt(address, &session, "msg_sol_subscription");
    let get =
        |address| json(address, "GET", &format!("/api/session/{session}"), "")["data"].clone();
    let row = get(address);
    assert_eq!(
        row["tokens"]["output"], 10.0,
        "subscription usage disappeared: {row}"
    );
    assert!(
        row.get("cost").is_none(),
        "API rates became a subscription charge: {row}"
    );
    assert_eq!(row["costStatus"], "unavailable");
    let page = json(
        address,
        "GET",
        &format!("/api/session/{session}/message?order=asc&diagnostics=cache"),
        "",
    );
    let calls = &page["data"]
        .as_array()
        .ok_or("history rows")?
        .iter()
        .find(|row| row["type"] == "assistant")
        .ok_or("assistant")?["metadata"]["cacheObservedCalls"];
    assert_eq!(calls["cost"]["subscription"], "unknown");
    assert_eq!(calls["cost"]["scope"], "observed-dispatch-api-equivalent");
    child.stop_after_witness(&root);
    drop(child);
    let mut child = spawn(&root, provider.address);
    let address = await_ready(&root, &mut child);
    assert_eq!(get(address)["costStatus"], "unavailable");
    assert!(get(address).get("cost").is_none());
    assert_eq!(provider.calls.load(Ordering::SeqCst), 1);
    child.stop_after_witness(&root);
    Ok(())
}

fn haiku_profile(root: &Path, priced: bool) -> Result<(), Box<dyn std::error::Error>> {
    prepare(root, 120_000);
    context_quality::configure_anthropic(root, 200_000);
    for name in ["model-catalog.json", "anthropic-account-models.json"] {
        let path = root.join(name);
        fs::write(
            &path,
            fs::read_to_string(&path)?.replace("claude-live", "claude-haiku-5-5"),
        )?;
    }
    if priced {
        let path = root.join("model-catalog.json");
        let mut catalog: serde_json::Value = serde_json::from_slice(&fs::read(&path)?)?;
        catalog["anthropic"]["models"]["claude-haiku-5-5"]["cost"] = serde_json::json!({
            "input":0.1,"output":0.5,"cache_read":0.01,"cache_write":0.125,
            "tiers":[{"tier":{"size":100000,"type":"context"},
                "input":0.5,"output":2.5,"cache_read":0.05,"cache_write":0.625}]
        });
        fs::write(path, serde_json::to_vec(&catalog)?)?;
    }
    Ok(())
}

fn prompt(address: SocketAddr, session: &str, id: &str) {
    json(
        address,
        "POST",
        &format!("/api/session/{session}/prompt"),
        &serde_json::json!({"id":id,"text":"continue","delivery":"queue","resume":true})
            .to_string(),
    );
    permission_residuals::wait_session_idle(address, session);
}

fn measured(fresh: u64, write: u64, read: u64, output: u64) -> Script {
    let stream = context_quality::anthropic_stream(
        &[serde_json::json!({"type":"text","text":"done"})],
        "end_turn",
        serde_json::json!({"input_tokens":fresh,"cache_creation_input_tokens":write,"cache_read_input_tokens":read}),
    );
    assert_eq!(
        stream.matches("\"output_tokens\":3").count(),
        1,
        "terminal fixture usage changed"
    );
    Script::Raw(stream.replace(
        "\"output_tokens\":3",
        &format!("\"output_tokens\":{output}"),
    ))
}

fn observed_anthropic_output(
    address: SocketAddr,
    session: &str,
    expected: &[u64],
) -> Result<(), Box<dyn std::error::Error>> {
    let page = json(
        address,
        "GET",
        &format!("/api/session/{session}/message?order=asc&limit=20"),
        "",
    );
    let mut outputs = Vec::new();
    for row in page["data"].as_array().ok_or("history rows absent")? {
        if row["type"] != "assistant" {
            continue;
        }
        let observed = &row["observedUsage"];
        // Anthropic includes thinking in output_tokens, without a separate
        // reasoning counter. Preserve that fact while projecting valid totals.
        assert!(
            observed.get("reasoning").is_none(),
            "a protocol-inapplicable counter became a reported value: {observed}"
        );
        outputs.push(
            observed["output"]
                .as_u64()
                .ok_or("reported output absent")?,
        );
    }
    assert_eq!(
        outputs.as_slice(),
        expected,
        "reported output changed: {page}"
    );
    Ok(())
}

fn listed_metadata(address: SocketAddr, first: &serde_json::Value) {
    let listed = json(address, "GET", "/api/session?limit=1", "");
    let row = &listed["data"][0];
    for field in [
        "id",
        "projectID",
        "title",
        "location",
        "model",
        "agent",
        "preset",
    ] {
        assert_eq!(row[field], first[field], "list metadata changed: {field}");
    }
    assert_eq!(row["time"]["created"], first["time"]["created"]);
    for field in ["tokens", "cost", "costStatus", "metrics"] {
        assert!(row.get(field).is_none(), "list replayed totals: {row}");
    }
}

fn inherited_anthropic_usage(
    address: SocketAddr,
    fork: &str,
    first: &serde_json::Value,
) -> Result<(), Box<dyn std::error::Error>> {
    let inherited = json(address, "GET", &format!("/api/session/{fork}"), "");
    assert_eq!(
        inherited["data"]["tokens"], first["tokens"],
        "inherited observations lost their valid aggregate: {inherited}"
    );
    observed_anthropic_output(address, fork, &[50, 24])
}

#[test]
fn api_key_haiku_catalog_cost_reproduces_the_windows_usage_and_reopens(
) -> Result<(), Box<dyn std::error::Error>> {
    let _journey = process_journey_custody();
    let root = root("session-cost-haiku");
    let _cleanup = terminal_coordination::RootCleanup(root.clone());
    haiku_profile(&root, true)?;
    let provider =
        ProviderFixture::start(vec![measured(4, 3604, 0, 50), measured(2, 131, 3604, 24)]);
    let mut child = spawn(&root, provider.address);
    let address = await_ready(&root, &mut child);
    let session = create(address);
    let (status, _) = request(
        address,
        "POST",
        &format!("/api/session/{session}/model"),
        r#"{"model":{"providerID":"anthropic","id":"claude-haiku-5-5"}}"#,
    );
    assert_eq!(status, 204);
    for id in ["msg_haiku_first", "msg_haiku_second"] {
        prompt(address, &session, id);
    }
    assert_eq!(provider.calls.load(Ordering::SeqCst), 2);
    let get =
        |address| json(address, "GET", &format!("/api/session/{session}"), "")["data"].clone();
    let first = get(address);
    assert_eq!(
        first["tokens"]["output"], 74.0,
        "fixture did not settle both real usage reports: {first}"
    );
    observed_anthropic_output(address, &session, &[50, 24])?;
    assert!(
        (first["cost"].as_f64().ok_or("session cost absent")? - 0.000540515).abs() < 1e-12,
        "wrong Session cost: {first}"
    );
    assert_eq!(first["costStatus"], "catalog-api-estimate");
    listed_metadata(address, &first);
    let forked = json(
        address,
        "POST",
        &format!("/api/session/{session}/fork"),
        "{}",
    );
    assert_eq!(forked["data"]["parentID"], session);
    let fork = forked["data"]["id"].as_str().ok_or("fork id absent")?;
    inherited_anthropic_usage(address, fork, &first)?;
    child.stop_after_witness(&root);
    drop(child);
    let mut child = spawn(&root, provider.address);
    let address = await_ready(&root, &mut child);
    let reopened = get(address);
    assert_eq!(
        reopened["tokens"], first["tokens"],
        "valid totals disappeared"
    );
    observed_anthropic_output(address, &session, &[50, 24])?;
    inherited_anthropic_usage(address, fork, &first)?;
    assert_eq!(reopened["cost"], first["cost"]);
    assert_eq!(reopened["costStatus"], first["costStatus"]);
    assert_eq!(
        provider.calls.load(Ordering::SeqCst),
        2,
        "reopen dispatched a request"
    );
    child.stop_after_witness(&root);
    drop(child);
    // Preserve both priced attempts, changing only the accounting owner's
    // assertion that its historical dispatch set is complete.
    let database = rusqlite::Connection::open(root.join("database.sqlite3"))?;
    assert_eq!(
        database.execute(
            "UPDATE provider_attempt_coverage SET complete=0 WHERE session_id=?1",
            [&session]
        )?,
        1
    );
    drop(database);
    for _ in 0..2 {
        let mut child = spawn(&root, provider.address);
        let address = await_ready(&root, &mut child);
        let partial = get(address);
        assert_eq!(partial["cost"], first["cost"], "priced rows changed");
        assert_eq!(partial["tokens"], first["tokens"], "observed usage changed");
        observed_anthropic_output(address, &session, &[50, 24])?;
        assert_eq!(
            partial["costStatus"], "catalog-api-subtotal",
            "incomplete dispatch coverage became complete: {partial}"
        );
        assert_eq!(
            provider.calls.load(Ordering::SeqCst),
            2,
            "reopen dispatched a request"
        );
        child.stop_after_witness(&root);
    }
    Ok(())
}

#[test]
fn missing_catalog_prices_keep_session_cost_unavailable() -> Result<(), Box<dyn std::error::Error>>
{
    let _journey = process_journey_custody();
    let root = root("session-cost-missing");
    let _cleanup = terminal_coordination::RootCleanup(root.clone());
    haiku_profile(&root, false)?;
    let provider = ProviderFixture::start(vec![measured(4, 0, 0, 50)]);
    let mut child = spawn(&root, provider.address);
    let address = await_ready(&root, &mut child);
    let session = create(address);
    let (status, _) = request(
        address,
        "POST",
        &format!("/api/session/{session}/model"),
        r#"{"model":{"providerID":"anthropic","id":"claude-haiku-5-5"}}"#,
    );
    assert_eq!(status, 204);
    prompt(address, &session, "msg_unpriced");
    let row = json(address, "GET", &format!("/api/session/{session}"), "");
    assert!(
        row["data"].get("cost").is_none(),
        "unknown cost became zero: {row}"
    );
    assert_eq!(row["data"]["costStatus"], "unavailable");
    assert_eq!(row["data"]["tokens"]["output"], 50.0);
    observed_anthropic_output(address, &session, &[50])?;
    child.stop_after_witness(&root);
    Ok(())
}

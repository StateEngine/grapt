//! Missing usage and an empty summary must not strand a live coding session.
use super::*;

#[path = "unmetered_compaction/corrections.rs"]
mod corrections;

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;
const WINDOW: u64 = 128_000;
const TURNS: usize = 32;
const SUMMARY_REQUEST: &str = "Output exactly the Markdown structure shown inside <template>";
const SEED: &str = "Read seed.rs and keep its existing behavior while reviewing later patches.";
const FOLLOWUP: &str = "UNMETERED_RECOVERY_FOLLOWUP";
const ANSWER: &str = "The later coding prompt was answered in the same session.";
const CALL: &str = "call_unmetered_seed";

fn unmetered(events: Vec<serde_json::Value>) -> Script {
    Script::Raw(
        events
            .into_iter()
            .map(|event| format!("data: {event}\n\n"))
            .collect(),
    )
}

fn text_without_usage(text: &str) -> Script {
    unmetered(vec![
        serde_json::json!({"type":"response.output_text.delta","delta":text}),
        serde_json::json!({"type":"response.output_text.done"}),
        serde_json::json!({"type":"response.completed","response":{}}),
    ])
}

fn response_for(request: &str) -> Script {
    if request.contains(SUMMARY_REQUEST) {
        // The summary comes back empty: it fails the compaction visibly.
        text_without_usage("")
    } else if request.contains(FOLLOWUP) {
        text_without_usage(ANSWER)
    } else {
        text_without_usage("The coding change was reviewed.")
    }
}

fn read_without_usage() -> Script {
    let item = "item_unmetered_seed";
    unmetered(vec![
        serde_json::json!({"type":"response.output_item.added","item":{
            "type":"function_call","id":item,"call_id":CALL,"name":"read"}}),
        serde_json::json!({"type":"response.function_call_arguments.delta","item_id":item,
            "delta":serde_json::json!({"path":"seed.rs"}).to_string()}),
        serde_json::json!({"type":"response.function_call_arguments.done","item_id":item}),
        serde_json::json!({"type":"response.completed","response":{}}),
    ])
}

fn history(address: SocketAddr, session: &str) -> serde_json::Value {
    json(
        address,
        "GET",
        &format!("/api/session/{session}/message?limit=200"),
        "",
    )
}

fn submit(address: SocketAddr, session: &str, id: &str, text: &str) {
    json(
        address,
        "POST",
        &format!("/api/session/{session}/prompt"),
        &serde_json::json!({"id":id,"text":text,"delivery":"queue","resume":true}).to_string(),
    );
}

fn run_prompt(
    address: SocketAddr,
    session: &str,
    provider: &ProviderFixture,
    id: &str,
    text: &str,
) -> serde_json::Value {
    let before = provider.calls.load(Ordering::SeqCst);
    submit(address, session, id, text);
    assert!(
        permission_residuals::wait_until(
            client_allowance::CLI_REQUEST_ALLOWANCE,
            permission_residuals::SETTLE_POLL,
            || provider.calls.load(Ordering::SeqCst) > before,
        )
        .is_some(),
        "a later input never reached the provider: {}",
        history(address, session)
    );
    permission_residuals::wait_session_idle(address, session);
    history(address, session)
}

fn summaries(provider: &ProviderFixture) -> usize {
    locked(&provider.bodies)
        .iter()
        .filter(|body| body.contains(SUMMARY_REQUEST))
        .count()
}

fn answers(page: &serde_json::Value) -> TestResult<usize> {
    let rows = page["data"].as_array().ok_or("history rows absent")?;
    Ok(rows
        .iter()
        .filter(|row| row["type"] == "assistant")
        .filter(|row| {
            row["content"].as_array().is_some_and(|parts| {
                parts
                    .iter()
                    .any(|part| part["type"] == "text" && part["text"] == ANSWER)
            })
        })
        .count())
}

fn assert_estimate_and_history(page: &serde_json::Value) -> TestResult {
    let rows = page["data"].as_array().ok_or("history rows absent")?;
    assert!(
        rows.iter()
            .any(|row| row["type"] == "user" && row["text"] == SEED),
        "request trimming removed durable user history"
    );
    assert!(
        rows.iter()
            .filter_map(|row| row["content"].as_array())
            .flatten()
            .any(|part| part["type"] == "tool"
                && part["id"] == CALL
                && part["state"]["status"] == "completed"),
        "request trimming removed the durable completed tool pair"
    );
    for row in rows.iter().filter(|row| row["type"] == "assistant") {
        assert!(
            row.get("tokens").is_none(),
            "omitted usage became reported tokens: {row}"
        );
    }
    let notices: Vec<_> = rows
        .iter()
        .filter(|row| row["type"] == "system" && row["metadata"]["contextTrimmed"] == true)
        .collect();
    assert!(
        !notices.is_empty(),
        "request trimming did not state its estimate: {page}"
    );
    for row in notices {
        assert!(row["text"]
            .as_str()
            .is_some_and(|text| text.starts_with("Context trimmed:")));
        let estimate = &row["metadata"]["contextEstimate"];
        assert_eq!(estimate["basis"], "encoded-request-bytes");
        assert_eq!(estimate["budgetBasis"], "admission-window");
        let count = |name: &str| -> TestResult<u64> {
            estimate[name]
                .as_u64()
                .or_else(|| estimate[name].as_str()?.parse().ok())
                .ok_or_else(|| format!("missing bounded count {name}: {estimate}").into())
        };
        let before = count("before")?;
        let after = count("after")?;
        let reserve = count("outputReserve")?;
        let effective_budget = WINDOW * 95 / 100;
        assert_eq!(count("budget")?, effective_budget);
        assert!(before > after && after > 0 && reserve > 0, "{estimate}");
        assert!(
            after
                .checked_add(reserve)
                .is_some_and(|total| total <= effective_budget),
            "{estimate}"
        );
    }
    Ok(())
}

#[test]
fn omitted_usage_and_an_empty_summary_do_not_strand_later_inputs() -> TestResult {
    let _journey = process_journey_custody();
    let root = root("unmetered-compaction-recovery");
    prepare(&root, 120_000);
    prepare_model_inventory(&root, WINDOW);
    set_compaction(&root, serde_json::json!({"auto":true,"buffer":4096}));
    let _cleanup = terminal_coordination::RootCleanup(root.clone());
    fs::write(
        root.join("seed.rs"),
        b"pub fn retained_fact() -> u32 { 7 }\n",
    )?;
    let mut scripts = vec![read_without_usage()];
    scripts.extend(vec![Script::ForRequest(response_for); TURNS * 4 + 8]);
    let provider = ProviderFixture::start(scripts);
    let mut child = process_fixture::spawn_controlled(&root, provider.address);
    let address = await_ready(&root, &mut child);
    let session = create(address);
    submit(address, &session, "msg_unmetered_seed", SEED);
    compaction_continuity::approve(address, &session, CALL);
    permission_residuals::wait_session_idle(address, &session);
    // Grow real promoted coding inputs until the product itself asks for a
    // summary. Do not depend on a particular tokenizer or calibration ratio.
    for turn in 0..TURNS {
        let text = format!(
            "Review coding patch {turn}:\n{}",
            "pub fn example(value: u32) -> u32 { value.saturating_add(1) }\n".repeat(400)
        );
        run_prompt(
            address,
            &session,
            &provider,
            &format!("msg_unmetered_{turn}"),
            &text,
        );
        if summaries(&provider) >= 1 {
            break;
        }
    }
    assert_eq!(
        summaries(&provider),
        1,
        "the session never reached the empty summary"
    );
    let settled = history(address, &session);
    assert!(
        !settled["data"]
            .as_array()
            .ok_or("history rows absent")?
            .iter()
            .any(|row| row["type"] == "compaction" && row["status"] == "completed"),
        "an empty summary replaced usable history: {settled}"
    );
    // Keep the same live process, Session, model and budget. Neither /compact
    // nor rewind is allowed to rescue this ordinary next-input path.
    let mut page = settled;
    for turn in 0..3 {
        let before = answers(&page)?;
        page = run_prompt(
            address,
            &session,
            &provider,
            &format!("msg_unmetered_after_{turn}"),
            &format!("{FOLLOWUP} {turn}: explain the reviewed change."),
        );
        assert_eq!(
            answers(&page)?,
            before + 1,
            "a later input was not answered: {page}"
        );
    }
    assert_estimate_and_history(&page)?;
    let bodies = locked(&provider.bodies);
    assert!(
        bodies.len() <= TURNS * 4 + 9,
        "compaction retried without a bound"
    );
    assert!(
        bodies.iter().all(|body| body.len() <= 4 * 1024 * 1024),
        "a request exceeded the bounded coding history fixture"
    );
    drop(bodies);
    child.stop_after_witness(&root);
    Ok(())
}

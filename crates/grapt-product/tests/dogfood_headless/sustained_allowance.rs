//! SUSTAINED-WORK-REPAIR-01 J02: one original input completes useful work
//! beyond the old 32-Step wall through the real product process, with no
//! synthetic user prompt and every tool call still individually approved.

use super::*;

const STEPS: usize = 40;

const CONSTRAINED_GROUP: [(&str, &str); 4] = [
    ("call_c01_constrained_1", "large-1.txt"),
    ("call_c01_constrained_2", "large-2.txt"),
    ("call_c01_constrained_3", "large-3.txt"),
    ("call_c01_constrained_4", "large-4.txt"),
];
#[cfg(not(windows))]
const RECOVERY_GROUP: [(&str, &str); 8] = [
    ("call_c02_recovery_1", "larger-1.txt"),
    ("call_c02_recovery_2", "larger-2.txt"),
    ("call_c02_recovery_3", "larger-3.txt"),
    ("call_c02_recovery_4", "larger-4.txt"),
    ("call_c02_recovery_5", "larger-5.txt"),
    ("call_c02_recovery_6", "larger-6.txt"),
    ("call_c02_recovery_7", "larger-7.txt"),
    ("call_c02_recovery_8", "larger-8.txt"),
];
const SUPPORTED_GROUP: [(&str, &str); 8] = [
    ("call_c01_supported_1", "larger-1.txt"),
    ("call_c01_supported_2", "larger-2.txt"),
    ("call_c01_supported_3", "larger-3.txt"),
    ("call_c01_supported_4", "larger-4.txt"),
    ("call_c01_supported_5", "larger-5.txt"),
    ("call_c01_supported_6", "larger-6.txt"),
    ("call_c01_supported_7", "larger-7.txt"),
    ("call_c01_supported_8", "larger-8.txt"),
];

fn approve_call(address: SocketAddr, session: &str, call: &str) {
    super::compaction_continuity::approve(address, session, call);
}

#[cfg(not(windows))]
fn provider_tool_output(body: &str, call: &str) -> String {
    let request: serde_json::Value = serde_json::from_str(body)
        .unwrap_or_else(|error| panic!("provider request was not JSON: {error}"));
    request["input"]
        .as_array()
        .and_then(|items| {
            items
                .iter()
                .find(|item| item["type"] == "function_call_output" && item["call_id"] == call)
        })
        .and_then(|item| item["output"].as_str())
        .unwrap_or_else(|| panic!("provider request omitted output for {call}"))
        .to_owned()
}

#[expect(clippy::too_many_lines, reason = "legacy")]
fn run_c01_budget_rebuild(
    label: &str,
    declared_context: u64,
    group: &[(&'static str, &'static str)],
    output_bytes: usize,
    expected_budget: u64,
) {
    let _journey = process_journey_custody();
    let root = root(label);
    prepare(&root, 500_000);
    prepare_model_inventory(&root, declared_context);
    fs::write(root.join("seed.txt"), text_lines(b's', 7_524))
        .unwrap_or_else(|error| panic!("seed fixture write failed: {error}"));
    for (_, path) in group {
        fs::write(root.join(path), text_lines(b'x', output_bytes))
            .unwrap_or_else(|error| panic!("large fixture write failed: {error}"));
    }
    let provider = ProviderFixture::start(vec![
        Script::ToolRead {
            call: "call_c01_seed",
            path: "seed.txt",
        },
        Script::ToolReads {
            calls: group.to_vec(),
        },
        Script::Output(5_645),
        Script::SuccessText("continued after budget-safe compaction"),
    ]);
    let mut child = spawn(&root, provider.address);
    let address = await_ready(&root, &mut child);
    let session = json(address, "POST", "/api/session", "{}");
    let id = session["data"]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("missing Session id"));
    json(
        address,
        "POST",
        &format!("/api/session/{id}/prompt"),
        r#"{"id":"msg_swr_c01_budget","text":"continue the original task after grouped evidence","delivery":"queue","resume":true}"#,
    );
    approve_call(address, id, "call_c01_seed");
    let calls: Vec<_> = group.iter().map(|(call, _)| *call).collect();
    super::compaction_continuity::approve_batch(address, id, &calls);
    permission_residuals::wait_session_idle(address, id);
    assert_eq!(
        provider.calls.load(Ordering::SeqCst),
        4,
        "the committed compaction did not admit the next useful coding request"
    );
    let bodies = locked(&provider.bodies).clone();
    assert!(
        bodies[2].contains("Output exactly the Markdown structure shown inside <template>"),
        "the oversized grouped Step skipped the ordinary compactor"
    );
    assert!(
        bodies[3].contains("Model-written continuation summary")
            && bodies[3].contains("continue the original task after grouped evidence"),
        "the admitted rebuild lost its summary or original user authority"
    );
    let messages = json(address, "GET", &format!("/api/session/{id}/message"), "");
    let rendered = messages.to_string();
    assert!(
        rendered.contains("continued after budget-safe compaction"),
        "the rebuilt coding Step did not complete: {rendered}"
    );
    assert!(!rendered.contains("ContextLimit"), "{rendered}");
    child.stop_after_witness(&root);
    drop(child);

    let store = EventStore::open(&root.join("database.sqlite3"), "swr-c01-diagnosis")
        .unwrap_or_else(|error| panic!("C01 evidence store is unreadable: {error}"));
    let session = id
        .parse::<SessionId>()
        .unwrap_or_else(|error| panic!("created Session identity is invalid: {error}"));
    let (_, _, descriptor) =
        grapt_store::compaction::latest_descriptor(store.connection(), &session)
            .unwrap_or_else(|error| panic!("C01 descriptor is unreadable: {error}"))
            .unwrap_or_else(|| panic!("ordinary budget journey committed no descriptor"));
    let grapt_types::RetainedHistory::DeterministicSelection {
        descriptor: selection,
    } = descriptor.retained()
    else {
        panic!("C01 descriptor has no deterministic selection")
    };
    let artifacts = FilesystemArtifactStore::open(
        root.join("dogfood-artifacts"),
        b"secode-live-headless1-artifacts-v1".to_vec(),
        Box::new(SystemClock),
    )
    .unwrap_or_else(|error| panic!("C01 artifacts are unreadable: {error}"));
    let page = artifacts
        .fetch(
            selection,
            0,
            ReadBudget::new(selection.length())
                .unwrap_or_else(|| panic!("C01 selection artifact is empty")),
        )
        .unwrap_or_else(|error| panic!("C01 selection fetch failed: {error}"));
    assert!(page.next_offset().is_none());
    let selection: serde_json::Value = serde_json::from_slice(page.bytes())
        .unwrap_or_else(|error| panic!("C01 selection is not JSON: {error}"));
    let number = |path: &[&str]| {
        path.iter()
            .fold(&selection, |value, key| &value[*key])
            .as_u64()
            .unwrap_or_else(|| panic!("C01 evidence number is absent at {path:?}"))
    };
    let original_estimate = number(&["admission", "estimate"]);
    let rebuilt_estimate = number(&["admission", "rebuiltEstimate"]);
    let rebuilt_with_reserve = rebuilt_estimate
        .checked_add(16_384)
        .and_then(|estimate| estimate.checked_add(4_096))
        .unwrap_or_else(|| panic!("C01 rebuilt request bound overflowed"));
    println!(
        "SWR_C01_EVIDENCE label={label} safe_cut={} source_head={} summary_request_budget={} original_estimate={original_estimate} rebuilt_estimate={rebuilt_estimate} output_reserve=16384 headroom=4096 removed_bytes={} retained_bytes={}",
        number(&["from"]),
        number(&["source_head"]),
        expected_budget,
        number(&["admission", "removedBytes"]),
        number(&["admission", "retainedBytes"]),
    );
    assert_eq!(selection["admission"]["trigger"], "policyThreshold");
    assert_eq!(selection["admission"]["budget"], expected_budget);
    assert!(
        original_estimate > expected_budget,
        "the fixture did not reproduce the named over-budget request: {selection}"
    );
    assert!(
        rebuilt_with_reserve < expected_budget,
        "the production rebuilt estimate plus the 16,384 output reserve and 4,096 headroom did not fit the coding budget: {selection}"
    );
    let grouped_bytes = u64::try_from(group.len().saturating_mul(output_bytes))
        .unwrap_or_else(|_| panic!("grouped output byte count exceeds u64"));
    assert!(
        selection["admission"]["removedBytes"]
            .as_u64()
            .is_some_and(|bytes| bytes >= grouped_bytes),
        "the safe cut did not remove the grouped outputs: {selection}"
    );
    assert!(
        selection["admission"]["retainedBytes"]
            .as_u64()
            .is_some_and(|bytes| bytes < expected_budget),
        "the published retained suffix is still larger than the coding budget: {selection}"
    );
    drop(store);
    process_fixture::cleanup(&root);
}

#[test]
fn swr_c01_constrained_profile_rebuilds_after_a_large_grouped_step() {
    // Leave room for path-dependent tool metadata in the rebuilt request.
    // The grouped outputs still exceed this constrained window by a wide margin.
    run_c01_budget_rebuild(
        "swr-c01-constrained",
        64_000,
        &CONSTRAINED_GROUP,
        28_000,
        60_800,
    );
}

#[test]
fn swr_c01_supported_profile_reproduces_the_same_larger_output_mechanism() {
    run_c01_budget_rebuild(
        "swr-c01-supported",
        128_000,
        &SUPPORTED_GROUP,
        36_000,
        121_600,
    );
}

#[cfg(not(windows))]
#[test]
#[cfg_attr(not(windows), expect(clippy::too_many_lines, reason = "legacy"))]
fn swr_c02_restart_recovers_exact_omission_and_compacts_another_grouped_step() {
    const OUTPUT_BYTES: usize = 36_000;
    let _journey = process_journey_custody();
    let root = root("swr-c02-recover-repeat");
    prepare(&root, 500_000);
    prepare_model_inventory(&root, 128_000);
    fs::write(root.join("seed.txt"), text_lines(b's', 7_524))
        .unwrap_or_else(|error| panic!("seed fixture write failed: {error}"));
    for (_, path) in SUPPORTED_GROUP {
        fs::write(root.join(path), text_lines(b'x', OUTPUT_BYTES))
            .unwrap_or_else(|error| panic!("large fixture write failed: {error}"));
    }
    let provider = ProviderFixture::start(vec![
        Script::ToolRead {
            call: "call_c02_seed",
            path: "seed.txt",
        },
        Script::ToolReads {
            calls: SUPPORTED_GROUP.to_vec(),
        },
        Script::Output(5_645),
        Script::SuccessText("first grouped Step continued"),
        Script::ToolHistoryInput {
            call: "call_c02_history_invalid",
            // An offset without a call remains invalid. Mixed call/cursor
            // arguments now correctly select output retrieval.
            arguments: serde_json::json!({"offset": 0}),
        },
        Script::ToolHistory {
            call: "call_c02_history",
            target: "call_c01_supported_1",
        },
        Script::ToolReads {
            calls: RECOVERY_GROUP.to_vec(),
        },
        Script::Output(5_645),
        Script::SuccessText("recovered exact output and continued after repeated compaction"),
    ]);
    let mut child = spawn(&root, provider.address);
    let address = await_ready(&root, &mut child);
    let created = json(address, "POST", "/api/session", "{}");
    let session = created["data"]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("missing Session id"));
    json(
        address,
        "POST",
        &format!("/api/session/{session}/prompt"),
        r#"{"id":"msg_swr_c02_first","text":"continue the grouped task","delivery":"queue","resume":true}"#,
    );
    approve_call(address, session, "call_c02_seed");
    let calls: Vec<_> = SUPPORTED_GROUP.iter().map(|(call, _)| *call).collect();
    super::compaction_continuity::approve_batch(address, session, &calls);
    permission_residuals::wait_session_idle(address, session);
    assert_eq!(provider.calls.load(Ordering::SeqCst), 4);
    child.stop_after_witness(&root);
    drop(child);

    let mut child = spawn(&root, provider.address);
    let address = await_ready(&root, &mut child);
    json(
        address,
        "POST",
        &format!("/api/session/{session}/prompt"),
        r#"{"id":"msg_swr_c02_recover","text":"recover the first omitted output, then continue the original task","delivery":"queue","resume":true}"#,
    );
    approve_call(address, session, "call_c02_history");
    let calls: Vec<_> = RECOVERY_GROUP.iter().map(|(call, _)| *call).collect();
    super::compaction_continuity::approve_batch(address, session, &calls);
    permission_residuals::wait_session_idle(address, session);
    assert_eq!(
        provider.calls.load(Ordering::SeqCst),
        9,
        "the restarted task did not reject invalid history, recover with a correction, compact again, and finish"
    );
    let bodies = locked(&provider.bodies).clone();
    let failed = provider_tool_output(&bodies[5], "call_c02_history_invalid");
    assert!(
        failed.contains("history offset requires a call identity"),
        "the valid correction did not receive actionable history preparation detail: {failed}"
    );
    let recovered: serde_json::Value =
        serde_json::from_str(&provider_tool_output(&bodies[6], "call_c02_history"))
            .unwrap_or_else(|error| panic!("history result was not JSON: {error}"));
    assert_eq!(recovered["call"], "call_c01_supported_1");
    assert_eq!(
        recovered["content"],
        numbered_text(b'x', OUTPUT_BYTES),
        "history did not return the exact omitted tool output"
    );
    assert!(
        bodies[7].contains("Output exactly the Markdown structure shown inside <template>"),
        "the second grouped Step skipped compaction"
    );
    assert!(
        bodies[8].contains("Model-written continuation summary")
            && bodies[8].contains("recover the first omitted output"),
        "the second rebuild lost its summary or current task authority"
    );
    let page = json(
        address,
        "GET",
        &format!("/api/session/{session}/message"),
        "",
    );
    let invalid = page["data"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|message| message["content"].as_array())
        .flatten()
        .find(|part| part["id"] == "call_c02_history_invalid")
        .unwrap_or_else(|| panic!("public history omitted the invalid history call: {page}"));
    assert_eq!(invalid["state"]["status"], "error");
    assert_eq!(
        invalid["state"]["error"]["message"],
        "tool preparation failed: history offset requires a call identity"
    );
    assert!(invalid["state"]["content"][0]["text"]
        .as_str()
        .is_some_and(|detail| detail.contains("history offset requires a call identity")));
    child.stop_after_witness(&root);
    drop(child);

    let store = EventStore::open(&root.join("database.sqlite3"), "swr-c02-repeat")
        .unwrap_or_else(|error| panic!("C02 evidence store is unreadable: {error}"));
    let compactions: i64 = store
        .connection()
        .query_row(
            "SELECT COUNT(*) FROM event WHERE aggregate_family = 'session'
             AND aggregate_id = ?1 AND type = 'session.compaction.ended.2'",
            [session],
            |row| row.get(0),
        )
        .unwrap_or_else(|error| panic!("C02 compaction count is unreadable: {error}"));
    assert_eq!(
        compactions, 2,
        "the joined journey did not compact both grouped Steps"
    );
    drop(store);
    process_fixture::cleanup(&root);
}

#[test]
fn swr_c02_a_summary_that_cannot_fit_commits_once_and_ends_the_step() {
    const OUTPUT_BYTES: usize = 28_000;
    let _journey = process_journey_custody();
    let root = root("swr-c02-no-progress");
    prepare(&root, 500_000);
    prepare_model_inventory(&root, 49_152);
    fs::write(root.join("seed.txt"), text_lines(b's', 7_524))
        .unwrap_or_else(|error| panic!("seed fixture write failed: {error}"));
    for (_, path) in CONSTRAINED_GROUP {
        fs::write(root.join(path), text_lines(b'x', OUTPUT_BYTES))
            .unwrap_or_else(|error| panic!("large fixture write failed: {error}"));
    }
    let provider = ProviderFixture::start(vec![
        Script::ToolRead {
            call: "call_c02_no_progress_seed",
            path: "seed.txt",
        },
        Script::ToolReads {
            calls: CONSTRAINED_GROUP.to_vec(),
        },
        Script::Output(30_000),
    ]);
    let mut child = spawn(&root, provider.address);
    let address = await_ready(&root, &mut child);
    let created = json(address, "POST", "/api/session", "{}");
    let session = created["data"]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("missing Session id"));
    json(
        address,
        "POST",
        &format!("/api/session/{session}/prompt"),
        r#"{"id":"msg_swr_c02_no_progress","text":"continue only if the rebuilt request fits","delivery":"queue","resume":true}"#,
    );
    approve_call(address, session, "call_c02_no_progress_seed");
    let calls: Vec<_> = CONSTRAINED_GROUP.iter().map(|(call, _)| *call).collect();
    super::compaction_continuity::approve_batch(address, session, &calls);
    permission_residuals::wait_session_idle(address, session);
    assert_eq!(
        provider.calls.load(Ordering::SeqCst),
        3,
        "a coding request opened after a summary that does not fit"
    );
    let messages = json(
        address,
        "GET",
        &format!("/api/session/{session}/message"),
        "",
    );
    assert!(
        messages.to_string().contains("executionFailure"),
        "the typed execution failure was not projected"
    );
    // A summary that does not fit is named so, but it is not an unusable
    // one: it leaves no compaction-failed notice.
    assert!(
        messages.to_string().contains("did not fit")
            && !messages.to_string().contains("compactionFailed"),
        "the stop was not named did-not-fit: {messages}"
    );
    child.stop_after_witness(&root);
    drop(child);

    let store = EventStore::open(&root.join("database.sqlite3"), "swr-c02-no-progress")
        .unwrap_or_else(|error| panic!("C02 evidence store is unreadable: {error}"));
    let committed: i64 = store
        .connection()
        .query_row(
            "SELECT COUNT(*) FROM event WHERE aggregate_family = 'session'
             AND aggregate_id = ?1 AND type = 'session.compaction.ended.2'",
            [session],
            |row| row.get(0),
        )
        .unwrap_or_else(|error| panic!("C02 descriptor count is unreadable: {error}"));
    // The summary is kept: the next input starts from it, trimmed.
    assert_eq!(committed, 1, "the summary was not committed exactly once");
    let failed: String = store
        .connection()
        .query_row(
            "SELECT data FROM event WHERE aggregate_family = 'session'
             AND aggregate_id = ?1 AND type = 'session.step.failed.3'
             ORDER BY seq DESC LIMIT 1",
            [session],
            |row| row.get(0),
        )
        .unwrap_or_else(|error| panic!("C02 terminal failure is unreadable: {error}"));
    assert!(
        failed.contains("compaction-no-progress"),
        "the durable failure is not readable as no-progress by an older build: {failed}"
    );
    drop(store);
    process_fixture::cleanup(&root);
}

#[test]
fn swr_j02_one_input_completes_beyond_thirty_two_steps_without_synthetic_prompts() {
    let _journey = process_journey_custody();
    let root = root("swr-step-budget");
    prepare(&root, 300_000);
    prepare_model_inventory(&root, 128_000);
    fs::write(root.join("tool.txt"), b"sustained verification")
        .unwrap_or_else(|error| panic!("fixture write failed: {error}"));
    let calls: Vec<&'static str> = (1..=STEPS)
        .map(|ordinal| &*Box::leak(format!("call_swr_{ordinal:02}").into_boxed_str()))
        .collect();
    let mut scripts: Vec<Script> = calls
        .iter()
        .map(|call| Script::ToolRead {
            call,
            path: "tool.txt",
        })
        .collect();
    scripts.push(Script::Success);
    let provider = ProviderFixture::start(scripts);
    let mut child = spawn(&root, provider.address);
    let address = await_ready(&root, &mut child);
    let session = json(address, "POST", "/api/session", "{}");
    let id = session["data"]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("missing Session id"));
    json(
        address,
        "POST",
        &format!("/api/session/{id}/prompt"),
        r#"{"id":"msg_swr_budget","text":"finish the long task","delivery":"queue","resume":true}"#,
    );
    // The provider can finish later calls before an earlier one is polled.
    // Observe the whole forty-call window so its terminals stay in the page.
    super::compaction_continuity::approve_batch(address, id, &calls);
    permission_residuals::wait_session_idle(address, id);
    assert_eq!(
        provider.calls.load(Ordering::SeqCst),
        STEPS + 1,
        "the one input did not reach its final answer"
    );
    let messages = json(address, "GET", &format!("/api/session/{id}/message"), "");
    let rendered = messages.to_string();
    assert!(
        rendered.contains("Hello from the live provider."),
        "{rendered}"
    );
    assert!(!rendered.contains("Execution failed"), "{rendered}");
    let users = messages["data"]
        .as_array()
        .unwrap_or_else(|| panic!("message list was not an array"))
        .iter()
        .filter(|row| row["type"] == "user")
        .count();
    assert_eq!(users, 1, "a synthetic user prompt continued the work");
    child.stop_after_witness(&root);
    drop(child);
    process_fixture::cleanup(&root);
}

#[cfg(not(windows))]
#[test]
#[cfg_attr(not(windows), expect(clippy::cognitive_complexity, reason = "legacy"))]
#[cfg_attr(not(windows), expect(clippy::too_many_lines, reason = "legacy"))]
fn swr_h2_dual_stream_shell_artifact_reaches_provider_and_survives_restart() {
    const CALL: &str = "call_swr_dual_stream";
    const COMMAND: &str = "i=0; while [ $i -lt 600 ]; do printf '%01024d' 0; printf '%01024d' 1 >&2; i=$((i+1)); done";
    const PAGE: usize = 64 * 1024;
    const HISTORY_CALLS: [&str; 17] = [
        "call_h2_page_0",
        "call_h2_page_1",
        "call_h2_page_2",
        "call_h2_page_3",
        "call_h2_page_4",
        "call_h2_page_5",
        "call_h2_page_6",
        "call_h2_page_7",
        "call_h2_page_8",
        "call_h2_page_9",
        "call_h2_page_10",
        "call_h2_page_11",
        "call_h2_page_12",
        "call_h2_page_13",
        "call_h2_page_14",
        "call_h2_page_15",
        "call_h2_page_16",
    ];
    let _journey = process_journey_custody();
    let root = root("swr-dual-stream");
    prepare(&root, 500_000);
    unattended(&root);
    prepare_model_inventory(&root, 2_000_000);
    let mut scripts = vec![Script::ToolShell {
        call: CALL,
        command: COMMAND,
    }];
    scripts.extend(HISTORY_CALLS.iter().enumerate().map(|(index, call)| {
        Script::ToolHistoryInput {
            call,
            arguments: serde_json::json!({"call": CALL, "offset": index * PAGE}),
        }
    }));
    scripts.extend([
        Script::SuccessText("bounded shell output reached the model"),
        Script::SuccessText("durable shell history survived restart"),
    ]);
    let provider = ProviderFixture::start(scripts);
    let mut child = spawn(&root, provider.address);
    let address = await_ready(&root, &mut child);
    let created = json(address, "POST", "/api/session", "{}");
    let session = created["data"]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("missing Session id"));
    json(
        address,
        "POST",
        &format!("/api/session/{session}/prompt"),
        r#"{"id":"msg_swr_dual_stream","text":"run both noisy streams","delivery":"queue","resume":true}"#,
    );
    approve_call(address, session, CALL);
    permission_residuals::wait_session_idle(address, session);
    let completed_calls = HISTORY_CALLS.len() + 2;
    assert_eq!(provider.calls.load(Ordering::SeqCst), completed_calls);
    let bodies = locked(&provider.bodies).clone();
    let preview = provider_tool_output(&bodies[1], CALL);
    assert!(preview.len() <= 32 * 1024);
    assert!(preview.contains("Shell output shortened for model context:"));
    assert!(preview.contains("history({\"call\":\"call_swr_dual_stream\",\"offset\":"));
    let preview_output = preview
        .find('{')
        .and_then(|start| serde_json::from_str::<serde_json::Value>(&preview[start..]).ok())
        .and_then(|receipt| {
            receipt
                .get("output")
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned)
        })
        .unwrap_or_else(|| preview.clone());
    assert!(
        preview_output.ends_with("Command exited with code 0.")
            || preview.contains("Command exited with code 0."),
        "unexpected preview tail: {}",
        &preview[preview.len().saturating_sub(160)..]
    );
    let artifact_bound = usize::try_from(grapt_runtime::TOOL_ARTIFACT_MAX_BYTES)
        .unwrap_or_else(|_| panic!("artifact bound exceeds usize"));
    assert_eq!(HISTORY_CALLS.len() * PAGE, artifact_bound);
    // History byte offsets address the durable receipt, including its JSON
    // overhead. Recover every encoded byte before interpreting command output.
    let mut encoded = String::new();
    let mut total = None;
    for (index, call) in HISTORY_CALLS.iter().enumerate() {
        let page: serde_json::Value =
            serde_json::from_str(&provider_tool_output(&bodies[completed_calls - 1], call))
                .unwrap_or_else(|error| panic!("history page was not JSON: {error}"));
        assert_eq!(page["call"], CALL);
        assert_eq!(page["offset"].as_u64(), Some((index * PAGE) as u64));
        let bytes = page["totalBytes"]
            .as_u64()
            .unwrap_or_else(|| panic!("history size missing"));
        assert!(bytes <= artifact_bound as u64 && bytes > (artifact_bound - PAGE) as u64);
        assert_eq!(*total.get_or_insert(bytes), bytes);
        let next = (((index + 1) * PAGE) < bytes as usize).then_some(((index + 1) * PAGE) as u64);
        assert_eq!(page["nextOffset"].as_u64(), next);
        encoded.push_str(
            page["content"]
                .as_str()
                .unwrap_or_else(|| panic!("history content absent")),
        );
    }
    assert_eq!(Some(encoded.len() as u64), total);
    let receipt: serde_json::Value = serde_json::from_str(&encoded)
        .unwrap_or_else(|error| panic!("recovered receipt was not complete JSON: {error}"));
    assert!(
        matches!(
            receipt["enforcement"]["kind"].as_str(),
            Some("platform_sandbox" | "harness_only")
        ),
        "unexpected enforcement kind: {:?}",
        receipt["enforcement"]
    );
    assert_eq!(receipt["ran"], true);
    let output = receipt["output"]
        .as_str()
        .unwrap_or_else(|| panic!("command output missing"));
    // With 64 KiB receipt/annotation headroom above the 1 MiB shell output cap,
    // the full 1 MiB dual-stream shell output survives intact inside the receipt.
    assert_eq!(output.len(), 1024 * 1024);
    assert!(receipt.get("omitted_output_bytes").is_none());
    assert!(output.starts_with("0000"), "stdout start was lost");
    assert!(output.contains('1'), "stderr end was lost");
    // Public message content remains its original excerpt, independent of the model preview.
    let page = json(
        address,
        "GET",
        &format!("/api/session/{session}/message"),
        "",
    );
    let public = page["data"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|message| message["content"].as_array())
        .flatten()
        .find(|part| part["id"] == CALL)
        .and_then(|part| part["state"]["content"][0]["text"].as_str())
        .unwrap_or_else(|| panic!("public history omitted the shell artifact"));
    assert!(!public.is_empty());
    let public: serde_json::Value = serde_json::from_str(public)
        .unwrap_or_else(|error| panic!("public execution receipt was not complete JSON: {error}"));
    assert_eq!(public["enforcement"], receipt["enforcement"]);
    assert_eq!(public["ran"], receipt["ran"]);
    let (output_part, status_part) = output
        .rsplit_once("\n\n")
        .unwrap_or_else(|| panic!("shell status paragraph absent"));
    assert_eq!(status_part, "Command exited with code 0.");
    // Supervision is a separate paragraph, never observed command bytes.
    const FALLBACK_NOTICE: &str = "[shell: this host has no cgroup containment; processes that leave the command's process group (setsid, daemons) may outlive it]";
    let separated_notice = format!("\n\n{FALLBACK_NOTICE}");
    let (command_part, fallback) = match output_part.strip_suffix(&separated_notice) {
        Some(command) => (command, true),
        None => (output_part, false),
    };
    match std::env::var("GRAPT_SHELL_EXPECT_CONTAINMENT").as_deref() {
        Ok("contained") => assert!(!fallback, "unexpected containment fallback"),
        Ok("fallback") => assert!(fallback, "fallback notice absent"),
        Err(std::env::VarError::NotPresent) => {}
        other => panic!("invalid containment witness mode: {other:?}"),
    }
    eprintln!("H2 shell containment: fallback={fallback}");
    assert_eq!(
        command_part.matches("[shell:").count(),
        0,
        "unexpected supervision notice inside command output"
    );
    // Detailed stdout/stderr capture markers survive intact when the receipt
    // fits within the 64 KiB headroom above the 1 MiB shell output cap.
    assert!(command_part.contains("[stdout output bounded: kept "));
    assert!(command_part.contains("[stderr output bounded: kept "));
    assert!(command_part.ends_with("of 614400 observed bytes]"));
    child.stop_after_witness(&root);
    drop(child);

    let mut restarted = spawn(&root, provider.address);
    let address = await_ready(&root, &mut restarted);
    json(
        address,
        "POST",
        &format!("/api/session/{session}/prompt"),
        r#"{"id":"msg_swr_dual_restart","text":"continue from durable history","delivery":"queue","resume":true}"#,
    );
    permission_residuals::wait_session_idle(address, session);
    assert_eq!(provider.calls.load(Ordering::SeqCst), completed_calls + 1);
    let bodies = locked(&provider.bodies).clone();
    assert_eq!(
        provider_tool_output(&bodies[completed_calls], CALL),
        preview
    );
    restarted.stop_after_witness(&root);
    drop(restarted);
    process_fixture::cleanup(&root);
}

#[path = "sustained_allowance/structural.rs"]
mod structural;

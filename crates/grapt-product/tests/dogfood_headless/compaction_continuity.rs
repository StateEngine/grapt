//! SESSION-CONTINUITY-01 J01: two successive compactions through the
//! production path, with instructions changed between them. Each committed
//! compaction is published live, projected into public history with its
//! window identity, and replayed byte-identically after a restart.

use super::*;

const OUTPUT_BYTES: usize = 36_000;
const FIRST_WINDOW: [(&str, &str); 8] = [
    ("call_j01_first_1", "j01-1.txt"),
    ("call_j01_first_2", "j01-2.txt"),
    ("call_j01_first_3", "j01-3.txt"),
    ("call_j01_first_4", "j01-4.txt"),
    ("call_j01_first_5", "j01-5.txt"),
    ("call_j01_first_6", "j01-6.txt"),
    ("call_j01_first_7", "j01-7.txt"),
    ("call_j01_first_8", "j01-8.txt"),
];
const SECOND_WINDOW: [(&str, &str); 8] = [
    ("call_j01_second_1", "j01-1.txt"),
    ("call_j01_second_2", "j01-2.txt"),
    ("call_j01_second_3", "j01-3.txt"),
    ("call_j01_second_4", "j01-4.txt"),
    ("call_j01_second_5", "j01-5.txt"),
    ("call_j01_second_6", "j01-6.txt"),
    ("call_j01_second_7", "j01-7.txt"),
    ("call_j01_second_8", "j01-8.txt"),
];
const FIRST_INSTRUCTION: &str = "continuity-instruction-epoch-first";
const SECOND_INSTRUCTION: &str = "continuity-instruction-epoch-second";
const SUMMARY_REQUEST: &str = "Output exactly the Markdown structure shown inside <template>";

/// Let a named call run, answering its request if it asks. An idle
/// Session that never ran the call is refused, not mistaken for completion.
pub(super) fn approve(address: SocketAddr, session: &str, call: &str) {
    approve_batch(address, session, &[call]);
}

/// Pace a bounded call window from one history snapshot per poll. Unasked
/// calls still need named terminal evidence; fetching the full page separately
/// for each call needlessly delays long compaction/restart witnesses.
pub(super) fn approve_batch(address: SocketAddr, session: &str, calls: &[&str]) {
    approve_batch_until(address, session, calls, PollDeadline::new());
}

fn approve_batch_until(address: SocketAddr, session: &str, calls: &[&str], wait: PollDeadline) {
    assert!(
        !calls.is_empty() && calls.len() <= 256,
        "approval witness window must contain 1..=256 calls"
    );
    let mut pending: std::collections::BTreeSet<&str> = calls.iter().copied().collect();
    assert_eq!(
        pending.len(),
        calls.len(),
        "duplicate approval witness call"
    );
    let mut attempt = 0;
    while wait.pending() {
        let asked = json(
            address,
            "GET",
            &format!("/api/session/{session}/permission"),
            "",
        );
        if let Some(rows) = asked["data"].as_array() {
            for row in rows {
                let Some(call) = row["source"]["callID"].as_str() else {
                    continue;
                };
                if !pending.contains(call) {
                    continue;
                }
                let id = row["id"]
                    .as_str()
                    .unwrap_or_else(|| panic!("missing permission id for {call}"));
                assert_eq!(
                    request(
                        address,
                        "POST",
                        &format!("/api/session/{session}/permission/{id}/reply"),
                        r#"{"reply":"once"}"#
                    )
                    .0,
                    204
                );
                pending.remove(call);
            }
        }
        if pending.is_empty() {
            return;
        }
        if attempt % 4 == 3 {
            let active = json(address, "GET", "/api/session/active", "")["data"]
                .as_object()
                .is_some_and(|active| active.contains_key(session));
            // Large windows need terminal evidence, not a full history page
            // on every active check. Still observe them while a later provider
            // response is held, and immediately when the Session becomes idle.
            // Single-call pacing and 50ms permission polling stay unchanged.
            if !active || calls.len() == 1 || attempt % 32 == 31 {
                observe_terminals(address, session, &mut pending);
                if pending.is_empty() {
                    return;
                }
            }
            if !active && attempt >= 40 {
                panic!(
                    "the Session went idle without running {}",
                    pending.iter().next().copied().unwrap_or("unnamed")
                );
            }
        }
        attempt += 1;
        thread::sleep(Duration::from_millis(50));
    }
    // An HTTP scan can consume the remaining allowance while the product
    // commits newer terminals. Refuse only from a fresh observation.
    observe_terminals(address, session, &mut pending);
    if pending.is_empty() {
        return;
    }
    panic!(
        "the chain stopped before {} ({wait})",
        pending.iter().next().copied().unwrap_or("unnamed")
    );
}

/// Decode terminal evidence without decoding unrelated frozen wire strings.
#[derive(Debug, serde::Deserialize)]
struct TerminalPage {
    data: Vec<TerminalMessage>,
    cursor: serde_json::Value,
}

#[derive(Debug, serde::Deserialize)]
struct TerminalMessage {
    content: Option<Vec<serde_json::Value>>,
}

/// Keep individual HTTP reads small even when a window contains many calls.
/// Exhausting the bounded scan leaves unseen calls pending; it never invents
/// completion. Cursors also let older named terminals survive a busy producer.
fn observe_terminals(
    address: SocketAddr,
    session: &str,
    pending: &mut std::collections::BTreeSet<&str>,
) {
    let mut cursor = None;
    let mut seen = std::collections::BTreeSet::new();
    for _ in 0..64 {
        let query = match &cursor {
            Some(cursor) => format!("limit=8&cursor={cursor}"),
            None => "order=desc&limit=8".to_owned(),
        };
        let (status, bytes) = request_with_read_timeout(
            address,
            "GET",
            &format!("/api/session/{session}/message?{query}"),
            "",
            client_allowance::CLI_REQUEST_ALLOWANCE,
        );
        assert_eq!(status, 200, "terminal history request failed");
        // The frozen recent string may contain a lone UTF-16 surrogate.
        // Ignore that unrelated field; its raw bytes are asserted by J03.
        let page: TerminalPage = serde_json::from_slice(&bytes)
            .unwrap_or_else(|error| panic!("terminal history failed: {error}"));
        let rows = &page.data;
        assert!(rows.len() <= 8, "terminal history exceeded its page bound");
        for part in rows
            .iter()
            .filter_map(|message| message.content.as_ref())
            .flatten()
        {
            if part["type"] == "tool"
                && matches!(
                    part["state"]["status"].as_str(),
                    Some("completed" | "error")
                )
            {
                if let Some(call) = part["id"].as_str() {
                    pending.remove(call);
                }
            }
        }
        if pending.is_empty() || rows.is_empty() {
            return;
        }
        let next = page.cursor["next"]
            .as_str()
            .filter(|next| {
                !next.is_empty()
                    && next.len() <= 1024
                    && next
                        .bytes()
                        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
            })
            .unwrap_or_else(|| panic!("terminal history omitted a valid next cursor: {page:?}"))
            .to_owned();
        assert!(
            seen.insert(next.clone()),
            "terminal history cursor repeated"
        );
        cursor = Some(next);
    }
}

#[path = "compaction_continuity/approval_tests.rs"]
mod approval_tests;

/// Runs one prompt whose grouped tool outputs force a compaction, and returns
/// the raw collected event stream observed while it ran, `data: ` framing and
/// all, for byte-level wire assertions.
pub(super) fn compact_once_raw(
    address: SocketAddr,
    session: &str,
    prompt: &str,
    group: &[(&str, &str)],
) -> String {
    let (stream, observed) = event_stream(address);
    let watcher =
        thread::spawn(move || collect(stream, observed, "\"type\":\"session.compaction.ended\""));
    json(
        address,
        "POST",
        &format!("/api/session/{session}/prompt"),
        prompt,
    );
    let calls: Vec<_> = group.iter().map(|(call, _)| *call).collect();
    approve_batch(address, session, &calls);
    let observed = watcher
        .join()
        .unwrap_or_else(|_| panic!("compaction event watcher panicked"));
    permission_residuals::wait_session_idle(address, session);
    observed
}

/// Runs one prompt whose grouped tool outputs force a compaction, and returns
/// the live `session.compaction.ended` notice observed while it ran.
pub(super) fn compact_once(
    address: SocketAddr,
    session: &str,
    prompt: &str,
    group: &[(&str, &str)],
) -> serde_json::Value {
    let observed = compact_once_raw(address, session, prompt, group);
    observed
        .lines()
        .filter_map(|line| line.strip_prefix("data: "))
        .filter_map(|encoded| serde_json::from_str::<serde_json::Value>(encoded).ok())
        .find(|event| event["type"] == "session.compaction.ended")
        .unwrap_or_else(|| panic!("no live compaction notice was published"))
}

/// Compaction rows in epoch order; the default history page is newest first.
pub(super) fn compactions(page: &serde_json::Value) -> Vec<serde_json::Value> {
    let mut rows = page["data"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|message| message["type"] == "compaction")
        .cloned()
        .collect::<Vec<_>>();
    rows.sort_by_key(|row| row["metadata"]["epoch"].as_u64());
    rows
}

/// The exact recent value the oracle publishes for the second window: the
/// user line, a blank-line separator, then the eight settled read pairs
/// joined by newlines, each result truncated at 2_000 UTF-16 units.
fn expected_second_recent() -> String {
    let mut parts = Vec::new();
    for index in 1..=8 {
        parts.push(format!(
            "[Assistant tool call]: read({{\"path\":\"j01-{index}.txt\"}})\n[Tool result]: {}\n[truncated]",
            numbered_prefix(b'x', OUTPUT_BYTES, 2_000)
        ));
    }
    format!("[User]: read the second window\n\n{}", parts.join("\n"))
}

#[test]
#[expect(clippy::too_many_lines, reason = "legacy")]
fn successive_compactions_refresh_instructions_and_replay_exactly() {
    let _journey = process_journey_custody();
    let root = root("j01-compaction-continuity");
    prepare(&root, 500_000);
    prepare_model_inventory(&root, 128_000);
    fs::write(root.join("seed.txt"), text_lines(b's', 7_524))
        .unwrap_or_else(|error| panic!("seed fixture write failed: {error}"));
    for (_, path) in FIRST_WINDOW {
        fs::write(root.join(path), text_lines(b'x', OUTPUT_BYTES))
            .unwrap_or_else(|error| panic!("large fixture write failed: {error}"));
    }
    let instructions = root.join("AGENTS.md");
    fs::write(&instructions, format!("Follow {FIRST_INSTRUCTION}.\n"))
        .unwrap_or_else(|error| panic!("instruction fixture write failed: {error}"));
    let provider = ProviderFixture::start(vec![
        Script::ToolReads {
            calls: FIRST_WINDOW.to_vec(),
        },
        Script::Output(5_645),
        Script::SuccessText("first window continued"),
        Script::ToolReads {
            calls: SECOND_WINDOW.to_vec(),
        },
        Script::Output(5_645),
        Script::SuccessText("second window continued"),
    ]);
    let mut child = spawn(&root, provider.address);
    let address = await_ready(&root, &mut child);
    let _approver = approval::Attachment::open(address, "*");
    let session = create(address);

    let first = compact_once(
        address,
        &session,
        r#"{"id":"msg_j01_first","text":"read the first window","delivery":"queue","resume":true}"#,
        &FIRST_WINDOW,
    );
    fs::write(&instructions, format!("Follow {SECOND_INSTRUCTION}.\n"))
        .unwrap_or_else(|error| panic!("instruction update failed: {error}"));
    let second = compact_once(
        address,
        &session,
        r#"{"id":"msg_j01_second","text":"read the second window","delivery":"queue","resume":true}"#,
        &SECOND_WINDOW,
    );
    assert_eq!(provider.calls.load(Ordering::SeqCst), 6);

    let bodies = locked(&provider.bodies).clone();
    assert!(
        bodies[1].contains(SUMMARY_REQUEST) && bodies[4].contains(SUMMARY_REQUEST),
        "each grouped Step did not run the production compactor"
    );
    assert!(
        bodies[2].contains(FIRST_INSTRUCTION),
        "the first rebuilt request lost its current instructions"
    );
    for (index, body) in bodies.iter().enumerate().skip(3) {
        if index == 4 {
            continue;
        }
        let request: serde_json::Value = serde_json::from_str(body)
            .unwrap_or_else(|error| panic!("request {index} is not JSON: {error}"));
        let input = request["input"]
            .as_array()
            .unwrap_or_else(|| panic!("request {index} has no input array"));
        let latest = input
            .iter()
            .filter(|item| item["role"] == "system")
            .filter_map(|item| item["content"].as_str())
            .rfind(|text| text.contains(FIRST_INSTRUCTION) || text.contains(SECOND_INSTRUCTION))
            .unwrap_or_else(|| panic!("request {index} lost its project instructions"));
        assert!(
            latest.contains(SECOND_INSTRUCTION) && !latest.contains(FIRST_INSTRUCTION),
            "request {index} did not make the new instruction value effective"
        );
        if body.contains(FIRST_INSTRUCTION) {
            assert!(
                latest.starts_with(
                    "Instruction source core/instructions replaces its previously supplied value:"
                ),
                "a retained baseline must have an explicit later replacement"
            );
        }
    }

    let path = format!("/api/session/{session}/message");
    let page = json(address, "GET", &path, "");
    let rows = compactions(&page);
    assert_eq!(
        rows.len(),
        2,
        "public history omitted a committed compaction: {page}"
    );
    let expected = [String::new(), expected_second_recent()];
    for (index, (row, notice)) in rows.iter().zip([&first, &second]).enumerate() {
        assert_eq!(row["status"], "completed");
        assert_eq!(row["reason"], "auto");
        assert_eq!(
            row["recent"].as_str(),
            Some(expected[index].as_str()),
            "the public history row diverges from the frozen oracle recent"
        );
        assert!(row["summary"].as_str().is_some_and(|text| !text.is_empty()));
        assert_eq!(
            notice["data"]["text"], row["summary"],
            "the live notice and public history disagree on the summary"
        );
        assert_eq!(
            notice["data"]["recent"].as_str(),
            Some(expected[index].as_str()),
            "the live notice and public history disagree on recent"
        );
        assert_eq!(
            notice["id"].as_str().and_then(|id| id.strip_prefix("evt_")),
            row["id"].as_str().and_then(|id| id.strip_prefix("msg_")),
            "the live notice and public history name different compactions"
        );
        assert_eq!(notice["data"]["sessionID"], session.as_str());
    }
    assert_ne!(
        rows[0]["metadata"]["contextWindowID"], rows[1]["metadata"]["contextWindowID"],
        "successive compactions reused one context window"
    );
    let epochs = rows
        .iter()
        .map(|row| {
            row["metadata"]["epoch"]
                .as_u64()
                .unwrap_or_else(|| panic!("compaction row has no epoch: {row}"))
        })
        .collect::<Vec<_>>();
    assert_eq!(epochs[1], epochs[0] + 1, "context epochs are not monotonic");
    assert!(
        first["durable"]["seq"].as_u64() < second["durable"]["seq"].as_u64(),
        "live compaction notices are not in durable order"
    );
    child.stop_after_witness(&root);
    drop(child);

    let mut restarted = spawn(&root, provider.address);
    let address = await_ready(&root, &mut restarted);
    let _approver = approval::Attachment::open(address, "*");
    assert_eq!(
        json(address, "GET", &path, ""),
        page,
        "public history did not replay the compacted Session exactly after restart"
    );
    restarted.stop_after_witness(&root);
    drop(restarted);
    process_fixture::cleanup(&root);
}

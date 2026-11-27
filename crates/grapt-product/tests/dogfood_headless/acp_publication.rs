//! ACP must finish a successful prompt when bounded public output needs hydration.
//! Each turn runs through the shared `AcpProcess` owner, which takes the CLI and
//! the product it embedded down through success, refusal, timeout or panic.
#![cfg(unix)]
use super::acp_process::{now, AcpObserved, AcpProcess, ACP_FRAMES};
use super::*;
use serde_json::{json as value, Value};

const PUBLIC_BYTES: usize = 256 * 1_024;
const TURN_ALLOWANCE: Duration = Duration::from_secs(60);

#[test]
#[cfg_attr(not(windows), expect(clippy::too_many_lines, reason = "legacy"))]
fn a_real_publication_gap_backfills_without_an_extra_terminal_close() {
    let observed = witness_observed_checked(
        "gap-terminal",
        1024,
        vec![
            Script::Raw({
                let arguments = value!({"command":r#"python3 -c 'from pathlib import Path; import time; gate=Path("gap-prefix-seen")
deadline=time.monotonic()+15
while not gate.exists() and time.monotonic()<deadline:
    time.sleep(0.01)
assert gate.exists(), "ACP prefix was not observed"
p=Path("gap-executions.log"); p.write_text(p.read_text()+"run\n"); print("z" * 300000)'"#, "timeout_ms":20_000});
                [
                    value!({"type":"response.output_text.delta","delta":"before tool\n"}),
                    value!({"type":"response.output_text.done"}),
                    value!({"type":"response.output_item.added","item":{"type":"function_call","id":"item_gap","call_id":"call_gap_output","name":"shell"}}),
                    value!({"type":"response.function_call_arguments.delta","item_id":"item_gap","delta":arguments.to_string()}),
                    value!({"type":"response.function_call_arguments.done","item_id":"item_gap"}),
                    value!({"type":"response.completed","response":{"usage":{"input_tokens":1,"output_tokens":1,"total_tokens":2}}}),
                ].iter().map(|event|format!("data: {event}\n\n")).collect()
            }),
            Script::Output(1024),
        ],
        Outcome::Completed,
        // Isolate publication loss from context compaction: the large tool
        // result remains legal input for this explicitly bounded test model.
        |root| {
            prepare_model_inventory(root, 1_000_000);
            fs::write(root.join("gap-executions.log"), "")
                .unwrap_or_else(|error| panic!("effect counter creation failed: {error}"));
        },
        |root, observed| {
            let effects = fs::read_to_string(root.join("gap-executions.log"))
                .unwrap_or_else(|error| panic!("effect counter missing: {error}"));
            assert_eq!(effects, "run\n", "recovery executed the shell tool again");
            assert_eq!(
                observed
                    .updates
                    .iter()
                    .zip(&observed.texts)
                    .find(|(kind, _)| *kind == "agent_message_chunk")
                    .map(|(_, text)| text.as_str()),
                Some("before tool\n"),
                "publication refusal lacked a delivered valid prefix"
            );
        },
    );
    let answer = observed
        .updates
        .iter()
        .zip(&observed.texts)
        .filter(|(kind, _)| *kind == "agent_message_chunk")
        .map(|(_, text)| text.as_str())
        .collect::<String>();
    assert_eq!(
        answer,
        format!("before tool\n{}", scripted_output(1024)),
        "gap backfill lost or duplicated assistant text"
    );
    let tools = observed
        .update_values
        .iter()
        .filter(|update| update["toolCallId"] == "call_gap_output")
        .collect::<Vec<_>>();
    assert_eq!(
        tools
            .iter()
            .filter(|update| update["sessionUpdate"] == "tool_call")
            .count(),
        1,
        "tool call duplicated: {tools:?}"
    );
    assert_eq!(
        tools
            .iter()
            .filter(|update| update["status"] == "completed")
            .count(),
        1,
        "missing or repeated tool final state: {tools:?}"
    );
    let final_text = tools
        .iter()
        .find(|update| update["status"] == "completed")
        .and_then(|update| update.pointer("/content/0/content/text"))
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("recovered final tool result missing"));
    let (prefix, notice) = final_text
        .rsplit_once("\n[output truncated:")
        .unwrap_or_else(|| panic!("final tool snapshot omitted the truncation marker"));
    // ACP clips the serialized public receipt inside this fixture's ASCII
    // output string. Close that partial string only to inspect its retained
    // fields; the actual ACP truncation marker still accounts for raw bytes.
    let partial: Value = serde_json::from_str(&format!("{prefix}\"}}"))
        .unwrap_or_else(|error| panic!("recovered receipt prefix was malformed: {error}"));
    assert_eq!(partial["enforcement"]["kind"], "harness_only");
    let omitted = partial["omitted_output_bytes"]
        .as_u64()
        .unwrap_or_else(|| panic!("public receipt loss count missing"));
    assert!(omitted > 0);
    let command = partial["output"]
        .as_str()
        .unwrap_or_else(|| panic!("recovered command prefix missing"));
    let marker = format!("\n[shell: omitted {omitted} output bytes to retain execution receipt]\n");
    let (head, tail) = command
        .split_once(&marker)
        .unwrap_or_else(|| panic!("public receipt loss marker disagrees with its count"));
    for segment in [head, tail] {
        assert!(
            !segment.is_empty() && segment.chars().all(|character| character == 'z'),
            "recovered command segment changed: {:?}",
            segment
                .chars()
                .filter(|character| *character != 'z')
                .take(128)
                .collect::<String>()
        );
    }
    let counts = notice
        .strip_prefix(" kept ")
        .and_then(|text| text.split_once(" of "))
        .unwrap_or_else(|| panic!("truncation marker lacks kept/total counts: {notice}"));
    let kept: usize = counts
        .0
        .parse()
        .unwrap_or_else(|error| panic!("kept count: {error}"));
    let total: usize = counts
        .1
        .split_once(" bytes;")
        .unwrap_or_else(|| panic!("total count missing"))
        .0
        .parse()
        .unwrap_or_else(|error| panic!("total count: {error}"));
    assert_eq!(kept, prefix.len());
    assert!(
        kept < total,
        "truncation marker did not describe truncation"
    );
    let ends = observed
        .stream_ends
        .lines()
        .map(|line| {
            serde_json::from_str::<Value>(line)
                .unwrap_or_else(|error| panic!("gap diagnostic unreadable: {error}"))
        })
        .collect::<Vec<_>>();
    assert!(
        !ends.is_empty(),
        "fixture did not cause a real publication gap"
    );
    assert!(
        ends.iter().all(|end| end["reason"]
            .as_str()
            .is_some_and(|reason| reason.contains("tool publication refused"))),
        "normal terminal added a stream close: {ends:?}"
    );
}

/// One ACP turn: initialize, a new Session and one prompt. The response is the
/// prompt's result, or its RPC error rendered as JSON.
fn prompt(root: &Path, provider: SocketAddr) -> (Result<Value, String>, AcpObserved) {
    let deadline = now() + TURN_ALLOWANCE;
    let mut acp = AcpProcess::start(root, provider, ACP_FRAMES);
    let mut observed = AcpObserved::default();
    let response = (|| {
        acp.request(
            1,
            "initialize",
            value!({"protocolVersion": 1, "clientCapabilities": {}}),
            deadline,
            &mut observed,
        )?;
        let created = acp.request(
            2,
            "session/new",
            value!({"cwd": root, "mcpServers": []}),
            deadline,
            &mut observed,
        )?;
        let session = created["sessionId"]
            .as_str()
            .ok_or_else(|| format!("ACP session creation refused: {created}"))?
            .to_owned();
        acp.request_observing(
            3,
            "session/prompt",
            value!({"sessionId": session,
                "prompt": [{"type": "text", "text": "reply with the scripted text"}]}),
            deadline,
            &mut observed,
            |row| {
                // Only the publication-gap fixture creates this counter.
                // The effect cannot produce its oversize result until the
                // actual host observes the acknowledged text prefix.
                if root.join("gap-executions.log").is_file()
                    && row
                        .pointer("/params/update/sessionUpdate")
                        .and_then(Value::as_str)
                        == Some("agent_message_chunk")
                    && row
                        .pointer("/params/update/content/text")
                        .and_then(Value::as_str)
                        == Some("before tool\n")
                {
                    fs::write(root.join("gap-prefix-seen"), "seen")
                        .unwrap_or_else(|error| panic!("prefix acknowledgement failed: {error}"));
                }
            },
        )
    })();
    // Prompt diagnostics end before this fixture closes protocol stdin. The
    // owned service stop has its own legitimate stream-shutdown diagnostic.
    observed.stream_ends = if root.join("gap-executions.log").is_file() {
        PollDeadline::within(Duration::from_secs(2))
            .poll(Duration::from_millis(10), || {
                let ends = stream_ends(root);
                (ends.ends_with('\n') && ends.contains("tool publication refused")).then_some(ends)
            })
            .unwrap_or_else(|| panic!("publication-gap diagnostic missing before owned cleanup"))
    } else {
        stream_ends(root)
    };
    acp.cleanup();
    observed.cleaned_up = acp.cleaned_up;
    let final_ends = stream_ends(root);
    let cleanup_ends = final_ends
        .strip_prefix(&observed.stream_ends)
        .unwrap_or_else(|| panic!("stream diagnostics changed during owned cleanup"));
    let cleanup_rows = cleanup_ends
        .lines()
        .map(|line| {
            serde_json::from_str::<Value>(line)
                .unwrap_or_else(|error| panic!("cleanup diagnostic unreadable: {error}"))
        })
        .collect::<Vec<_>>();
    assert!(
        cleanup_rows.len() <= 1
            && cleanup_rows.iter().all(|row| {
                row["schema"] == "grapt.stream-end.v1"
                    && row["cause"] == "reconcile"
                    && row["reason"] == "service-shutdown"
                    && row["reconcile_reason"].is_null()
            }),
        "owned cleanup added a non-shutdown stream close: {cleanup_rows:?}"
    );
    (response, observed)
}

fn stream_ends(root: &Path) -> String {
    use std::io::Read;
    let file = match fs::File::open(root.join("diagnostics/event-stream.jsonl")) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return String::new(),
        Err(error) => panic!("stream diagnostics unreadable: {error}"),
    };
    let mut text = String::new();
    file.take(1024 * 1024 + 1)
        .read_to_string(&mut text)
        .unwrap_or_else(|error| panic!("stream diagnostics unreadable: {error}"));
    assert!(text.len() <= 1024 * 1024, "stream diagnostics exceed 1 MiB");
    text
}

enum Outcome {
    Completed,
    ProviderRefusal,
}

fn witness(label: &str, bytes: usize, script: Script, outcome: Outcome) {
    witness_steps(label, bytes, vec![script], outcome);
}

/// One provider call per Step; returns the kinds of live updates ACP received.
fn witness_steps(label: &str, bytes: usize, scripts: Vec<Script>, outcome: Outcome) -> Vec<String> {
    witness_configured(label, bytes, scripts, outcome, |_| {})
}

/// As [`witness_steps`], after `configure` adjusts the prepared service root.
fn witness_configured(
    label: &str,
    bytes: usize,
    scripts: Vec<Script>,
    outcome: Outcome,
    configure: impl FnOnce(&Path),
) -> Vec<String> {
    witness_observed(label, bytes, scripts, outcome, configure).updates
}

/// The assistant text an ACP client received for one scripted answer (every
/// agent_message_chunk, in order) and why the product ended its live stream.
fn received_answer(label: &str, bytes: usize, script: Script) -> (String, String) {
    let observed = witness_observed(label, bytes, vec![script], Outcome::Completed, |_| {});
    let chunks: Vec<&str> = observed
        .updates
        .iter()
        .zip(&observed.texts)
        .filter(|(kind, _)| *kind == "agent_message_chunk")
        .map(|(_, text)| text.as_str())
        .collect();
    (chunks.concat(), observed.stream_ends)
}

/// The answer `Script::Output(bytes)` streams.
fn scripted_output(bytes: usize) -> String {
    format!(
        "{OUTPUT_OBJECTIVE}{}",
        "x".repeat(bytes - OUTPUT_OBJECTIVE.len())
    )
}

/// As [`witness_configured`], returning everything the client observed.
fn witness_observed(
    label: &str,
    bytes: usize,
    scripts: Vec<Script>,
    outcome: Outcome,
    configure: impl FnOnce(&Path),
) -> AcpObserved {
    witness_observed_checked(label, bytes, scripts, outcome, configure, |_, _| {})
}

fn witness_observed_checked(
    label: &str,
    bytes: usize,
    scripts: Vec<Script>,
    outcome: Outcome,
    configure: impl FnOnce(&Path),
    check: impl FnOnce(&Path, &AcpObserved),
) -> AcpObserved {
    let _journey = process_journey_custody();
    let root = root(&format!("acp-publication-{label}"));
    prepare(&root, 90_000);
    configure(&root);
    fs::write(root.join("README.md"), "the file a tool step reads\n")
        .unwrap_or_else(|error| panic!("fixture file failed: {error}"));
    let steps = scripts.len();
    let provider = ProviderFixture::start(scripts);
    let (response, observed) = prompt(&root, provider.address);
    let calls = provider.calls.load(Ordering::SeqCst);
    let updates = observed
        .updates
        .iter()
        .filter(|kind| *kind == "agent_message_chunk")
        .count();
    eprintln!("ACP_PUBLICATION_EVIDENCE label={label} bytes={bytes} calls={calls} updates={updates} cleaned_up={} response={response:?}", observed.cleaned_up);
    eprintln!("ACP_STREAM_ENDS label={label}\n{}", observed.stream_ends);
    assert!(
        observed.cleaned_up,
        "the ACP CLI or its embedded product outlived the turn; root={}",
        root.display()
    );
    assert_eq!(calls, steps, "provider calls; root={}", root.display());
    match outcome {
        Outcome::Completed => {
            let stop = response
                .as_ref()
                .map(|result| result["stopReason"].clone())
                .unwrap_or_else(|error| {
                    panic!("ACP prompt refused: {error}; root={}", root.display())
                });
            assert_eq!(stop, "end_turn", "root={}", root.display());
            assert!(
                updates > 0,
                "ACP completed without live assistant updates; root={}",
                root.display()
            );
        }
        Outcome::ProviderRefusal => {
            let error = response
                .err()
                .unwrap_or_else(|| panic!("oversized provider frame did not refuse"));
            let error: Value = serde_json::from_str(&error)
                .unwrap_or_else(|_| panic!("refusal is not an RPC error: {error}"));
            assert_eq!(error["code"], -32603, "{error}");
            assert_eq!(updates, 0, "illegal provider frame emitted assistant text");
        }
    }
    check(&root, &observed);
    process_fixture::cleanup(&root);
    observed
}

/// Legal 16 KiB provider deltas reach the public accumulation boundary. The
/// block's ended event cannot carry the whole text in an SSE frame there, so
/// it is sent elided (length and digest, by ruling) and the
/// live stream no longer ends mid-turn. The client receives the WHOLE answer
/// (it once received 0 to 12 of 16 deltas and no more).
#[test]
fn acp_text_at_public_bound_completes_with_the_whole_answer() {
    let (received, ends) =
        received_answer("fragmented-at", PUBLIC_BYTES, Script::Output(PUBLIC_BYTES));
    assert_eq!(
        received.len(),
        PUBLIC_BYTES,
        "the client did not receive the whole answer"
    );
    assert!(
        received == scripted_output(PUBLIC_BYTES),
        "the received answer differs"
    );
    // The ended frame went out elided, so nothing was refused for size: the
    // stream neither drained and ended (reconcile) nor closed at once
    // (invalidate), as it did when clients saw no text at all.
    assert!(
        !ends.contains("public content bound exceeded"),
        "the at-bound stream was ended for size: {ends}"
    );
    assert!(
        !ends.contains(r#""cause":"invalidate""#),
        "the at-bound stream was invalidated: {ends}"
    );
}

/// Above the public bound the client receives the public projection, the
/// first PUBLIC_BYTES of the answer, as history serves every client.
#[test]
fn acp_text_above_public_bound_completes_with_the_public_projection() {
    let (received, _) = received_answer(
        "fragmented-above",
        PUBLIC_BYTES + 1,
        Script::Output(PUBLIC_BYTES + 1),
    );
    assert_eq!(
        received.len(),
        PUBLIC_BYTES,
        "the client did not receive the public projection"
    );
    assert!(
        received == scripted_output(PUBLIC_BYTES + 1)[..PUBLIC_BYTES],
        "the received answer differs from its public projection"
    );
}

#[test]
fn acp_oversized_provider_frame_refuses_before_live_text() {
    // AdapterState::MAX_BYTES also bounds SseDecoder input. This is an
    // invalid-provider-output control, not a public-transport regression.
    let delta = value!({"type":"response.output_text.delta","delta":"x".repeat(PUBLIC_BYTES + 1)});
    let response = format!(
        "data: {delta}\n\n\
         data: {{\"type\":\"response.output_text.done\"}}\n\n\
         data: {{\"type\":\"response.completed\",\"response\":{{\"usage\":{{\"input_tokens\":1,\"output_tokens\":1,\"total_tokens\":2}}}}}}\n\n"
    );
    witness(
        "single-above-refusal",
        PUBLIC_BYTES + 1,
        Script::Raw(response),
        Outcome::ProviderRefusal,
    );
}

fn event(body: Value) -> String {
    format!("data: {body}\n\n")
}

fn completed() -> String {
    event(value!({"type":"response.completed", "response":{"usage":{
        "input_tokens":1, "output_tokens":1, "total_tokens":2
    }}}))
}

#[test]
fn acp_tiny_delta_burst_completes_with_live_updates() {
    // One unpaced HTTP body; every individual SSE event is within the bound.
    let delta = event(value!({"type":"response.output_text.delta", "delta":"token "}));
    let response =
        delta.repeat(5_000) + &event(value!({"type":"response.output_text.done"})) + &completed();
    witness(
        "tiny-burst",
        30_000,
        Script::Raw(response),
        Outcome::Completed,
    );
}

#[test]
fn acp_reasoning_and_completed_item_before_text_deltas_completes() {
    let text = "A short streamed answer.";
    let events = [
        value!({"type":"response.output_item.added", "output_index":0,
            "item":{"id":"reasoning_1","type":"reasoning", "summary":[],
                "encrypted_content":"e".repeat(4_096)}}),
        value!({"type":"response.output_item.done", "output_index":0,
            "item":{"id":"reasoning_1","type":"reasoning", "summary":[],
                "encrypted_content":"e".repeat(4_096)}}),
        value!({"type":"response.output_item.added", "output_index":1,
            "item":{"id":"message_1","type":"message","role":"assistant",
                "content":[]}}),
        value!({"type":"response.output_item.done", "output_index":1,
            "item":{"id":"message_1","type":"message","role":"assistant",
                "status":"completed", "content":[{"type":"output_text","text":text}]}}),
        value!({"type":"response.output_text.delta", "item_id":"message_1",
            "output_index":1, "content_index":0, "delta":text}),
        value!({"type":"response.output_text.done", "item_id":"message_1",
            "output_index":1, "content_index":0, "text":text}),
    ];
    let response = events.into_iter().map(event).collect::<String>() + &completed();
    witness(
        "reasoning-completed-item",
        text.len(),
        Script::Raw(response),
        Outcome::Completed,
    );
}

/// Benchmark R01/R02/C01: tool Steps completed, then the run died at the
/// first text of the final Step. Every public text above ran in Step one.
fn text_deltas(count: usize) -> String {
    let delta = event(value!({"type":"response.output_text.delta", "delta":"token "}));
    delta.repeat(count) + &event(value!({"type":"response.output_text.done"})) + &completed()
}

#[test]
fn acp_text_after_a_tool_step_completes_with_live_updates() {
    witness_steps(
        "text-after-tool",
        6 * 96,
        vec![
            Script::ToolRead {
                call: "call_read_readme",
                path: "README.md",
            },
            Script::Raw(text_deltas(96)),
        ],
        Outcome::Completed,
    );
}

#[test]
fn acp_tool_step_reaches_acp_as_live_tool_updates() {
    let updates = witness_steps(
        "tool-live",
        6 * 4,
        vec![
            Script::ToolRead {
                call: "call_read_live",
                path: "README.md",
            },
            Script::Raw(text_deltas(4)),
        ],
        Outcome::Completed,
    );
    let tools = updates
        .iter()
        .filter(|kind| matches!(kind.as_str(), "tool_call" | "tool_call_update"))
        .count();
    eprintln!("ACP_PUBLICATION_EVIDENCE label=tool-live tool_updates={tools}");
    assert!(
        tools > 0,
        "the tool Step reached ACP with no live tool update"
    );
}

/// Every ae5ce3a8 row ran with reasoning effort on and
/// had zero live updates. Reasoning precedes the call and the answer, in
/// `parts` summary parts as a real Responses stream sends them.
fn reasoning(kind: &str, parts: usize, deltas: usize) -> String {
    let mut body = String::new();
    for part in 0..parts {
        for _ in 0..deltas {
            body.push_str(&event(value!({"type":format!("response.{kind}.delta"),
                "item_id":"reasoning_1", "output_index":0, "summary_index":part,
                "delta":"thinking "})));
        }
        body.push_str(&event(value!({"type":format!("response.{kind}.done"),
            "item_id":"reasoning_1", "output_index":0, "summary_index":part})));
    }
    body
}

fn reasoned_tool_read(kind: &str, call: &str) -> String {
    let arguments = value!({"path":"README.md"}).to_string();
    reasoning(kind, 2, 8)
        + &event(value!({"type":"response.output_item.added", "item":{
            "type":"function_call", "id":format!("item_{call}"), "call_id":call, "name":"read"}}))
        + &event(value!({"type":"response.function_call_arguments.delta",
            "item_id":format!("item_{call}"), "delta":arguments}))
        + &event(value!({"type":"response.function_call_arguments.done",
            "item_id":format!("item_{call}")}))
        + &completed()
}

fn reasoned_text_after_tool(label: &str, kind: &str) -> Vec<String> {
    witness_steps(
        label,
        6 * 96,
        vec![
            Script::Raw(reasoned_tool_read(kind, "call_reasoned_read")),
            Script::Raw(reasoning(kind, 2, 8) + &text_deltas(96)),
        ],
        Outcome::Completed,
    )
}

#[test]
fn acp_reasoning_summary_steps_complete_with_live_updates() {
    reasoned_text_after_tool("reasoning-summary", "reasoning_summary_text");
}

#[test]
fn acp_reasoning_text_steps_complete_with_live_updates() {
    reasoned_text_after_tool("reasoning-text", "reasoning_text");
}

/// Benchmark ae5ce3a8: a Session with no stored effort runs at the account
/// default. The live Step opening must agree with the durable one, or every
/// live update is refused and each text delta ends the event stream (fixed
/// in 37b36c6a; this is its red).
#[test]
fn acp_account_default_effort_keeps_live_updates() {
    let updates = witness_configured(
        "default-effort",
        6 * 96,
        vec![
            Script::ToolRead {
                call: "call_default_effort",
                path: "README.md",
            },
            Script::Raw(text_deltas(96)),
        ],
        Outcome::Completed,
        |root| {
            fs::write(
                root.join("account-models.json"),
                value!({"models":[{"slug":"gpt-live", "visibility":"list",
                    "context_window":128_000, "default_reasoning_level":"medium",
                    "supported_reasoning_levels":[
                        {"effort":"low"}, {"effort":"medium"}, {"effort":"high"}]}]})
                .to_string(),
            )
            .unwrap_or_else(|error| panic!("reasoning inventory failed: {error}"));
        },
    );
    assert!(
        updates.iter().any(|kind| kind == "tool_call"),
        "the default-effort tool Step had no live tool update"
    );
}

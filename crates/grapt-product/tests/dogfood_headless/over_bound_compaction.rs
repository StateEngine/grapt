//! A Session far past the read bounds compacts in ONE summary pass over its
//! newest window, and the next input runs on the compacted history. The
//! history is seeded settled (no failure): it is simply too large to read.
use super::*;
use grapt_artifact::{ArtifactKind, CaptureBound, Sensitivity};
use grapt_provider::{AdapterState, AttemptFragment, FinishReason, ToolExecution};
use grapt_runtime::{AttemptOwner, DurableAttemptOwner, DurableFact, NoLiveSubscribers};
use grapt_types::{PhysicalAttemptId, StepNumber, ToolCallId, ToolTerminalArtifact};

type TestResult = Result<(), Box<dyn std::error::Error>>;
const SUMMARY: &str = "Output exactly the Markdown structure shown inside <template>";
const OBJECTIVE: &str = "## Objective\n- continue the user task; the seed files were already read";
const ORIGINAL: &str = "ORIGINAL-TASK: preserve the complete recovery contract";

/// Settled tool calls, `groups` Steps of `calls`, each output one captured value.
fn prehistory(root: &Path, session: &str, shape: (u32, u32), output: &str) -> TestResult {
    let (groups, calls) = shape;
    let session: SessionId = session.parse()?;
    let artifacts = || {
        FilesystemArtifactStore::open(
            root.join("dogfood-artifacts"),
            b"secode-live-headless1-artifacts-v1".to_vec(),
            Box::new(SystemClock),
        )
    };
    let store = Arc::new(Mutex::new(EventStore::open(
        &root.join("database.sqlite3"),
        "live-headless1",
    )?));
    let captured = artifacts()?.capture(
        std::io::Cursor::new(output.as_bytes()),
        CaptureBound::new(128 * 1024, 4096).ok_or("capture bound")?,
        ArtifactKind::CapturedOutput,
        Sensitivity::Ordinary,
    )?;
    let (clock, ids) = (SystemClock, OsRandomIds::seed()?);
    for group in 0..groups {
        let mut owner = DurableAttemptOwner::elect(
            store.clone(),
            artifacts()?,
            &clock,
            &ids,
            NoLiveSubscribers,
            session.clone(),
            StepNumber::new(100 + group).ok_or("step")?,
            None,
        )?;
        owner
            .commit(DurableFact::AttemptStarted {
                attempt: PhysicalAttemptId::new(format!("pat_over_{group}")).ok_or("attempt")?,
            })
            .map_err(|error| error.reason)?;
        for index in 0..calls {
            let call = ToolCallId::new(format!("call_over_{group}_{index}")).ok_or("call")?;
            owner
                .commit(DurableFact::Fragment {
                    fragment: Box::new(AttemptFragment::ToolCalled {
                        call: call.clone(),
                        tool: "read".into(),
                        input: r#"{"path":"fixture.txt"}"#.into(),
                        execution: ToolExecution::Local,
                        state: AdapterState::none(),
                    }),
                })
                .map_err(|error| error.reason)?;
            owner
                .commit(DurableFact::ToolTerminal {
                    call,
                    outcome: grapt_types::ToolTerminalOutcome::Completed {
                        output: Some(
                            ToolTerminalArtifact::new(
                                captured.reference().clone(),
                                captured.disposition(),
                            )
                            .ok_or("output")?,
                        ),
                    },
                })
                .map_err(|error| error.reason)?;
        }
        owner
            .commit(DurableFact::StepEnded {
                finish: FinishReason::ToolCalls,
            })
            .map_err(|error| error.reason)?;
    }
    Ok(())
}

fn send(address: SocketAddr, session: &str, id: &str, text: &str) {
    json(
        address,
        "POST",
        &format!("/api/session/{session}/prompt"),
        &serde_json::json!({"id":id,"text":text,"delivery":"queue","resume":true}).to_string(),
    );
}

fn carried(root: &Path, address: SocketAddr, session: &str, input: &str) {
    manual_compaction::assert_carried(root, input, "session.execution.succeeded.1");
    permission_residuals::wait_session_idle(address, session);
}

/// Seed settled Steps under one original input, compact
/// on request, and run one more input. Exactly one summary call is made, it
/// quotes the newest history (never the oldest call), and the input's request
/// carries the labelled summary, the original task and the input itself.
fn compacts_in_one_pass(
    label: &str,
    shape: (u32, u32),
    output: &str,
    refuse_first: bool,
) -> TestResult {
    let groups = shape.0;
    let _journey = process_journey_custody();
    let root = root(label);
    prepare(&root, 120_000);
    prepare_model_inventory(&root, 80_000);
    set_compaction(&root, serde_json::json!({"auto":false,"server_side":false}));
    REFUSE_FIRST.store(refuse_first, Ordering::SeqCst);
    let scripts = if refuse_first {
        // The provider refuses the first summary as a context overflow.
        let summary = Script::ForRequest(|body| {
            if !body.contains(SUMMARY) {
                Script::Success
            } else if REFUSE_FIRST.swap(false, Ordering::SeqCst) {
                Script::Refused {
                    status: 400,
                    body: r#"{"error":{"type":"invalid_request_error","code":"context_length_exceeded","message":"maximum context length exceeded"}}"#,
                }
            } else {
                Script::SuccessText(OBJECTIVE)
            }
        });
        vec![Script::Success, summary.clone(), summary.clone(), summary]
    } else {
        vec![
            Script::Success,
            Script::SuccessText(OBJECTIVE),
            Script::Success,
        ]
    };
    let provider = ProviderFixture::start(scripts);
    let mut child = spawn(&root, provider.address);
    let address = await_ready(&root, &mut child);
    let session = create(address);
    send(address, &session, "msg_over_original", ORIGINAL);
    carried(&root, address, &session, "msg_over_original");
    child.stop_after_witness(&root);
    drop(child);
    prehistory(&root, &session, shape, output)?;
    let mut child = spawn(&root, provider.address);
    let address = await_ready(&root, &mut child);
    json(
        address,
        "POST",
        &format!("/api/session/{session}/compact"),
        "{}",
    );
    send(
        address,
        &session,
        "msg_over_next",
        "continue after compaction",
    );
    carried(&root, address, &session, "msg_over_next");
    let bodies = locked(&provider.bodies);
    let summaries: Vec<_> = bodies
        .iter()
        .filter(|body| body.contains(SUMMARY))
        .collect();
    let expected = if refuse_first { 2 } else { 1 };
    assert_eq!(summaries.len(), expected, "summary calls");
    if refuse_first {
        // The retry quotes less of the oldest history than the refused call.
        assert!(
            summaries[1].len() < summaries[0].len(),
            "the retry did not drop any history"
        );
    }
    let summaries = &summaries[summaries.len() - 1..];
    assert!(
        !summaries[0].contains("call_over_0_0"),
        "the oldest call stayed in the quote"
    );
    // The newest Steps stay exact after the cut; the quote holds the window
    // before them, so some call of the later half is in it.
    assert!(
        (groups / 2..groups).any(|group| summaries[0].contains(&format!("call_over_{group}_"))),
        "the newest window is not in the quote"
    );
    let request = bodies.last().ok_or("no request")?;
    for exact in [
        "Model-written continuation summary",
        ORIGINAL,
        "continue after compaction",
    ] {
        assert!(request.contains(exact), "the next request lost {exact}");
    }
    assert_eq!(
        provider.calls.load(Ordering::SeqCst),
        if refuse_first { 4 } else { 3 }
    );
    drop(bodies);
    child.stop_after_witness(&root);
    drop(child);
    process_fixture::cleanup(&root);
    Ok(())
}

static REFUSE_FIRST: AtomicBool = AtomicBool::new(false);

/// A summary the provider refuses as a context overflow is made again with
/// less of the oldest history, and the compaction commits.
#[test]
fn a_summary_refused_as_an_overflow_is_retried_with_less_history() -> TestResult {
    compacts_in_one_pass(
        "over-bound-retry",
        (41, 32),
        "small historical result",
        true,
    )
}

/// 1,281 settled calls: past the 1,024 identity bound, small outputs.
#[test]
fn a_session_past_the_identity_bound_compacts_in_one_pass_and_runs_the_next_input() -> TestResult {
    compacts_in_one_pass(
        "over-bound-identity",
        (41, 32),
        "small historical result",
        false,
    )
}

/// 608 calls of 8 KiB: past the 4 MiB declared-bytes bound, under the row bound.
#[test]
fn a_session_past_the_declared_bytes_bound_compacts_in_one_pass_and_runs_the_next_input(
) -> TestResult {
    compacts_in_one_pass(
        "over-bound-declared",
        (38, 16),
        &"x".repeat(8 * 1024),
        false,
    )
}

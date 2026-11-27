//! Durable history setup only, while the disposable service is stopped.
use super::*;
use grapt_artifact::{ArtifactKind, CaptureBound, Sensitivity};
use grapt_provider::{AdapterState, AttemptFragment, FinishReason, ToolExecution};
use grapt_runtime::{AttemptOwner, DurableAttemptOwner, DurableFact, NoLiveSubscribers};
use grapt_types::{PhysicalAttemptId, StepNumber, ToolCallId, ToolTerminalArtifact};

pub(super) fn outputs(
    root: &Path,
    session: &str,
    count: usize,
) -> Result<(), Box<dyn std::error::Error>> {
    seed(root, session, count, 1).map(|_| ())
}

pub(super) fn oversized_row(
    root: &Path,
    session: &str,
) -> Result<String, Box<dyn std::error::Error>> {
    seed(root, session, 1, 45)?
        .pop()
        .ok_or_else(|| "missing oversized row".into())
}

fn seed(
    root: &Path,
    session: &str,
    count: usize,
    parts: usize,
) -> Result<Vec<String>, Box<dyn std::error::Error>> {
    assert!(count <= 45 && parts <= 45);
    let mut messages = Vec::new();
    let session: SessionId = session.parse()?;
    let store = Arc::new(Mutex::new(EventStore::open(
        &root.join("database.sqlite3"),
        "live-headless1",
    )?));
    let artifacts = FilesystemArtifactStore::open(
        root.join("dogfood-artifacts"),
        b"secode-live-headless1-artifacts-v1".to_vec(),
        Box::new(SystemClock),
    )?;
    let output = format!("API01_OUTPUT{}", "x".repeat(190 * 1024));
    let captured = artifacts.capture(
        std::io::Cursor::new(output.as_bytes()),
        CaptureBound::new(256 * 1024, 64 * 1024).ok_or("capture bound")?,
        ArtifactKind::CapturedOutput,
        Sensitivity::Ordinary,
    )?;
    let clock = SystemClock;
    let ids = OsRandomIds::seed()?;
    for index in 0..count {
        let owner_artifacts = FilesystemArtifactStore::open(
            root.join("dogfood-artifacts"),
            b"secode-live-headless1-artifacts-v1".to_vec(),
            Box::new(SystemClock),
        )?;
        let mut owner = DurableAttemptOwner::elect(
            store.clone(),
            owner_artifacts,
            &clock,
            &ids,
            NoLiveSubscribers,
            session.clone(),
            StepNumber::new(100 + 100 * u32::try_from(parts)? + u32::try_from(index)?)
                .ok_or("step")?,
            None,
        )?;
        owner
            .commit(DurableFact::AttemptStarted {
                attempt: PhysicalAttemptId::new(format!("pat_api01_{parts}_{index}"))
                    .ok_or("attempt")?,
            })
            .map_err(|error| error.reason)?;
        messages.push(owner.message().ok_or("opened message")?.as_str().to_owned());
        for part in 0..parts {
            let call = ToolCallId::new(format!("call_api01_seeded_{parts}_{index}_{part}"))
                .ok_or("call")?;
            owner
                .commit(DurableFact::Fragment {
                    fragment: Box::new(AttemptFragment::ToolCalled {
                        call: call.clone(),
                        tool: "web_search".into(),
                        input: r#"{"query":"fixture"}"#.into(),
                        execution: ToolExecution::ProviderHosted,
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
    Ok(messages)
}

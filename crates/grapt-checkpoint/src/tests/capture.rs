use super::support::*;
use crate::{CheckpointSelector, RescueSupport};
use grapt_types::{
    BranchName, CaptureRefusalReason, CheckpointCaptureOutcome, CheckpointHeadId,
    CheckpointLineage, PathIdentity, VcsBaseline, VcsUnavailableReason, WorktreeIdentity,
};
use std::sync::{Arc, Mutex};

fn authority() -> FakeAuthority {
    FakeAuthority(Arc::new(Mutex::new(AuthorityMode::Current)))
}

#[test]
fn stable_capture_resolve_inspect_and_fork_cross_the_provider_boundary(
) -> Result<(), Box<dyn std::error::Error>> {
    let root = TestRoot::new("capture-complete")?;
    let workspace = "C:/disposable/checkpoint-workspace";
    let observation = symbolic_observation(workspace, HEAD_A)?;
    let state = Arc::new(Mutex::new(ProviderState::new()));
    let mut engine = engine(
        Arc::clone(&state),
        RescueSupport::Supported,
        ScriptedObserver::repeated(observation),
        root.journal()?,
        authority(),
        TestClock::new(1_700_000_000_000),
        Arc::new(TestIds::default()),
    )?;

    let negotiated = engine.negotiate(&provider_version()?, vec![lane("files")?])?;
    assert_eq!(negotiated.lanes(), &[lane("files")?]);

    let receipt = engine.capture(
        operation("capture-1")?,
        PathIdentity::canonical(workspace),
        vec![lane("files")?],
    )?;
    let checkpoint = match receipt.outcome() {
        CheckpointCaptureOutcome::Captured { anchor, lineage } => {
            assert!(anchor.fidelity().is_exact());
            assert_eq!(anchor.baseline(), receipt.before().baseline());
            assert_eq!(receipt.before(), receipt.after());
            assert_eq!(lineage, &CheckpointLineage::Root);
            assert_eq!(anchor.worktree(), receipt.before().worktree());
            anchor.checkpoint().clone()
        }
        other => return Err(format!("capture was not exact: {other:?}").into()),
    };

    let head = CheckpointHeadId::new("main-head").ok_or("head")?;
    let resolved = engine.resolve(CheckpointSelector::Head {
        provider: provider_id()?,
        version: provider_version()?,
        head: head.clone(),
    })?;
    assert_eq!(resolved, checkpoint);
    let inspected = engine.inspect(checkpoint.clone())?;
    assert_eq!(inspected.reference(), &checkpoint);
    assert_eq!(inspected.provider().lanes(), &[lane("files")?]);

    let fork = engine.fork(operation("fork-1")?, checkpoint.clone(), head)?;
    assert_eq!(fork.parent(), &checkpoint);
    assert_ne!(fork.child(), fork.parent());

    let rendered = format!("{engine:?} {receipt:?}");
    assert!(!rendered.contains("SECRET-CHECKPOINT-PAYLOAD"));
    assert!(state
        .lock()
        .map_err(|_| "state")?
        .secret_payload
        .starts_with("SECRET"));
    Ok(())
}

#[test]
fn a_head_change_after_provider_capture_refuses_the_anchor(
) -> Result<(), Box<dyn std::error::Error>> {
    let root = TestRoot::new("capture-head-change")?;
    let workspace = "C:/disposable/head-change";
    let state = Arc::new(Mutex::new(ProviderState::new()));
    let mut engine = engine(
        state,
        RescueSupport::Supported,
        ScriptedObserver::sequence(vec![
            symbolic_observation(workspace, HEAD_A)?,
            symbolic_observation(workspace, HEAD_B)?,
        ])?,
        root.journal()?,
        authority(),
        TestClock::new(1),
        Arc::new(TestIds::default()),
    )?;
    let receipt = engine.capture(
        operation("capture-moved")?,
        PathIdentity::canonical(workspace),
        vec![lane("files")?],
    )?;
    assert!(matches!(
        receipt.outcome(),
        CheckpointCaptureOutcome::Refused {
            reason: CaptureRefusalReason::HeadChangedDuringCapture
        }
    ));
    assert_ne!(receipt.before().baseline(), receipt.after().baseline());
    Ok(())
}

#[test]
fn a_worktree_identity_change_after_capture_refuses_the_anchor(
) -> Result<(), Box<dyn std::error::Error>> {
    let root = TestRoot::new("capture-worktree-change")?;
    let workspace = "C:/disposable/worktree-change";
    let mut relinked = symbolic_observation(workspace, HEAD_A)?;
    relinked = crate::WorkspaceObservation::new(
        WorktreeIdentity::Git {
            root: PathIdentity::canonical(workspace),
            common_dir: PathIdentity::canonical("C:/other/common"),
            git_dir: PathIdentity::canonical("C:/other/worktree"),
        },
        relinked.baseline().clone(),
    );
    let mut engine = engine(
        Arc::new(Mutex::new(ProviderState::new())),
        RescueSupport::Supported,
        ScriptedObserver::sequence(vec![symbolic_observation(workspace, HEAD_A)?, relinked])?,
        root.journal()?,
        authority(),
        TestClock::new(1),
        Arc::new(TestIds::default()),
    )?;
    let receipt = engine.capture(
        operation("capture-relinked")?,
        PathIdentity::canonical(workspace),
        vec![lane("files")?],
    )?;
    assert!(
        matches!(
            receipt.outcome(),
            CheckpointCaptureOutcome::Refused {
                reason: CaptureRefusalReason::WorktreeChangedDuringCapture
            }
        ),
        "WCP09: a relinked worktree must never receive the old anchor"
    );
    Ok(())
}

#[test]
fn non_git_unborn_detached_and_unavailable_are_four_observable_states(
) -> Result<(), Box<dyn std::error::Error>> {
    let workspace = "C:/disposable/vcs-states";
    let cases = vec![
        ("plain", plain_observation(workspace), None),
        (
            "unborn",
            crate::WorkspaceObservation::new(
                WorktreeIdentity::Git {
                    root: PathIdentity::canonical(workspace),
                    common_dir: PathIdentity::canonical("C:/git/common"),
                    git_dir: PathIdentity::canonical("C:/git/worktree"),
                },
                VcsBaseline::GitUnborn {
                    branch: BranchName::new("main").ok_or("branch")?,
                },
            ),
            Some(CaptureRefusalReason::UnbornGitRoot),
        ),
        ("detached", detached_observation(workspace)?, None),
        (
            "unavailable",
            crate::WorkspaceObservation::new(
                WorktreeIdentity::Plain {
                    root: PathIdentity::canonical(workspace),
                },
                VcsBaseline::Unavailable {
                    reason: VcsUnavailableReason::MetadataUnreadable,
                },
            ),
            Some(CaptureRefusalReason::VcsUnavailable {
                reason: VcsUnavailableReason::MetadataUnreadable,
            }),
        ),
    ];

    for (name, observation, expected_refusal) in cases {
        let root = TestRoot::new(&format!("capture-state-{name}"))?;
        let mut engine = engine(
            Arc::new(Mutex::new(ProviderState::new())),
            RescueSupport::Supported,
            ScriptedObserver::repeated(observation),
            root.journal()?,
            authority(),
            TestClock::new(1),
            Arc::new(TestIds::default()),
        )?;
        let receipt = engine.capture(
            operation(&format!("capture-{name}"))?,
            PathIdentity::canonical(workspace),
            vec![lane("files")?],
        )?;
        match expected_refusal {
            None => assert!(matches!(
                receipt.outcome(),
                CheckpointCaptureOutcome::Captured { .. }
            )),
            Some(expected) => assert!(matches!(
                receipt.outcome(),
                CheckpointCaptureOutcome::Refused { reason } if reason == &expected
            )),
        }
    }
    Ok(())
}

#[test]
fn unknown_lane_and_wrong_root_fail_without_calling_the_provider(
) -> Result<(), Box<dyn std::error::Error>> {
    let root = TestRoot::new("capture-input-refusals")?;
    let workspace = "C:/disposable/input-refusals";
    let state = Arc::new(Mutex::new(ProviderState::new()));
    let mut engine = engine(
        Arc::clone(&state),
        RescueSupport::Supported,
        ScriptedObserver::repeated(symbolic_observation("C:/somewhere-else", HEAD_A)?),
        root.journal()?,
        authority(),
        TestClock::new(1),
        Arc::new(TestIds::default()),
    )?;
    let unknown = engine.capture(
        operation("capture-unknown")?,
        PathIdentity::canonical(workspace),
        vec![lane("secret-payload")?],
    )?;
    assert!(matches!(
        unknown.outcome(),
        CheckpointCaptureOutcome::Refused {
            reason: CaptureRefusalReason::UnknownLane { .. }
        }
    ));
    let wrong_root = engine.capture(
        operation("capture-wrong-root")?,
        PathIdentity::canonical(workspace),
        vec![lane("files")?],
    )?;
    assert!(matches!(
        wrong_root.outcome(),
        CheckpointCaptureOutcome::Refused {
            reason: CaptureRefusalReason::WrongRoot
        }
    ));
    assert_eq!(state.lock().map_err(|_| "state")?.capture_count, 0);
    Ok(())
}

#[test]
fn provider_unavailability_is_a_refusal_not_an_absent_checkpoint(
) -> Result<(), Box<dyn std::error::Error>> {
    let root = TestRoot::new("capture-provider-refusal")?;
    let workspace = "C:/disposable/provider-refusal";
    let state = Arc::new(Mutex::new(ProviderState::new()));
    state.lock().map_err(|_| "state")?.capture_mode = CaptureMode::Refuse;
    let mut engine = engine(
        state,
        RescueSupport::Supported,
        ScriptedObserver::repeated(symbolic_observation(workspace, HEAD_A)?),
        root.journal()?,
        authority(),
        TestClock::new(1),
        Arc::new(TestIds::default()),
    )?;
    let receipt = engine.capture(
        operation("capture-provider-refusal")?,
        PathIdentity::canonical(workspace),
        vec![lane("files")?],
    )?;
    assert!(matches!(
        receipt.outcome(),
        CheckpointCaptureOutcome::Refused {
            reason: CaptureRefusalReason::ProviderUnavailable
        }
    ));
    Ok(())
}

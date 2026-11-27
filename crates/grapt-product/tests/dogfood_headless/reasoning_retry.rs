//! SC-REL-02c: a drop after finished reasoning retries; the model re-thinks.
use super::*;

#[test]
fn sc_rel_02c_a_drop_after_finished_reasoning_retries_the_same_request() {
    let _journey = process_journey_custody();
    let root = root("reasoning-retry");
    let _cleanup = terminal_coordination::RootCleanup(root.clone());
    prepare(&root, 150_000);
    let provider = ProviderFixture::start(vec![
        Script::FinishedReasoningThenDrop,
        Script::SuccessText("answered after re-thinking"),
    ]);
    let mut child = spawn(&root, provider.address);
    let address = await_ready(&root, &mut child);
    let session = create(address);
    let service = root.to_string_lossy().into_owned();
    let run = super::cli_continuity::cli(
        &root,
        &[
            "run",
            "--service",
            &service,
            "--session",
            &session,
            "--prompt",
            "go",
        ],
    );
    assert_eq!(
        run.code,
        Some(0),
        "the retried Step must succeed: {}",
        run.stdout
    );
    assert_eq!(
        provider.calls.load(Ordering::SeqCst),
        2,
        "exactly one retry"
    );
    {
        let bodies = locked(&provider.bodies);
        assert_eq!(
            bodies[0], bodies[1],
            "the retry must resend the dropped attempt's request unchanged"
        );
    }
    let history = json(
        address,
        "GET",
        &format!("/api/session/{session}/message?order=asc&limit=50"),
        "",
    )
    .to_string();
    assert!(history.contains("answered after re-thinking"), "{history}");
    assert!(!history.contains("first thought"), "{history}");
    assert!(!history.contains("half an answer"), "{history}");
    child.stop_after_witness(&root);
}

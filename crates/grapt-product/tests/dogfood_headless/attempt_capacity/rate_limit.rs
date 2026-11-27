//! Real HTTP classification must shrink child admission and successful SSE must recover it.
use super::*;

#[test]
fn child_http_rate_limit_storm_shrinks_and_recovers_without_failing_children() {
    let _journey = process_journey_custody();
    let root = root("capacity-rate-limit-storm");
    let _cleanup = terminal_coordination::RootCleanup(root.clone());
    prepare(&root, 200_000);
    attended(&root);
    one_slot(&root);
    child_slots(&root, 3);
    let entered: Vec<_> = (0..3).map(|_| Arc::new(Phase::new())).collect();
    let release: Vec<_> = (0..3).map(|_| Arc::new(Phase::new())).collect();
    let mut scripts = vec![children::delegated()];
    for _ in 0..3 {
        scripts.push(Script::Refused {
            status: 429,
            body: r#"{"error":{"type":"rate_limit_exceeded"}}"#,
        });
    }
    for index in 0..3 {
        scripts.push(Script::ControlledTerminalOutput {
            bytes: 64,
            entered: entered[index].clone(),
            release: release[index].clone(),
            disconnect: Arc::new(AtomicBool::new(false)),
        });
    }
    scripts.push(Script::SuccessText(
        "all children recovered from provider backpressure",
    ));
    let provider = ProviderFixture::start(scripts);
    let mut process = spawn(&root, provider.address);
    let address = await_ready(&root, &mut process);
    let _approver = approval::Attachment::open(address, "*");
    let parent = create(address);
    prompt(
        address,
        &parent,
        "msg_capacity_rate_storm",
        "delegate three tasks and recover rate limits",
    );
    for index in 0..3 {
        let ask = permission_boundaries::pending(
            address,
            &parent,
            &format!("call_capacity_child_{index}"),
        );
        permission_boundaries::reply(address, &parent, &ask, "once");
    }
    entered[0]
        .wait("first child retry", &provider.stop)
        .unwrap_or_else(|error| panic!("{error}"));
    // Without the real HTTP -> capacity hook, all three retry streams enter.
    thread::sleep(Duration::from_millis(100));
    assert_eq!(
        provider.calls.load(Ordering::SeqCst),
        5,
        "rate-limited children did not reduce concurrency"
    );
    release[0].release();
    entered[1]
        .wait("second retry after success", &provider.stop)
        .unwrap_or_else(|error| panic!("{error}"));
    // Neither remaining retry is released: both can enter only after recovery
    // raises the one-slot penalty to two through the real normalized terminal.
    entered[2]
        .wait("third retry alongside the second", &provider.stop)
        .unwrap_or_else(|error| panic!("{error}"));
    assert_eq!(provider.calls.load(Ordering::SeqCst), 7);
    release[1].release();
    release[2].release();
    manual_compaction::assert_carried(
        &root,
        "msg_capacity_rate_storm",
        "session.execution.succeeded.1",
    );
    assert_eq!(provider.calls.load(Ordering::SeqCst), 8);
    assert_eq!(children::children(address, &parent).len(), 3);
    provider
        .settle_terminal_workers()
        .unwrap_or_else(|error| panic!("{error}"));
    process.stop_after_witness(&root);
}

//! The real terminal's quiet resume avoids baseline reads; a restart takes them.
use super::*;

fn resumed(address: SocketAddr) -> u64 {
    json(address, "GET", "/api/health", "")["replays"]["resumed"]
        .as_u64()
        .unwrap_or_else(|| panic!("health did not expose its resume counter"))
}

fn baseline(request: &str, session: &str) -> bool {
    request.starts_with("GET /api/permission/request")
        || request.starts_with(&format!("GET /api/session/{session}/snapshot"))
        || request.starts_with(&format!("GET /api/session/{session}/message"))
}

#[test]
fn a_quiet_native_cut_certifies_and_a_restart_reads_a_fresh_baseline() {
    let _journey = process_journey_custody();
    let provider =
        ProviderFixture::start_tolerating_cuts(vec![Script::ForRequest(busy_script); 32]);
    let mut run = Run::start("tui-native-resume", provider.address);
    let before = resumed(run.address);
    run.relay.trace_start();
    assert!(run.relay.cut() > 0, "the TUI had a live stream to cut");
    assert!(
        run.tui
            .wait_until(RECOVERY, |_| resumed(run.address) > before)
            .is_some(),
        "a quiet native cut never certified"
    );
    // Raw input remains serviced while the backend consumes its certificate.
    probe(&run.tui, 4, Duration::from_millis(20), 0);
    let quiet = run.relay.requests();
    eprintln!("quiet native reconnect: {quiet:?}");
    assert!(quiet.iter().any(|line| line.starts_with("GET /api/event?")));
    assert!(
        quiet.iter().all(|line| !baseline(line, &run.session)),
        "certified native reconnect added a baseline read: {quiet:?}"
    );
    run.submitted.push(submit(&run.tui, 1, Stream::Normal));
    require_fresh(
        &run.tui,
        &[TICK],
        0,
        Duration::from_secs(20),
        "stream after certification",
    );
    run.relay.trace_start();
    run.restart_service();
    let permission_path = "GET /api/permission/request";
    let snapshot_path = format!("GET /api/session/{}/snapshot", run.session);
    let fresh = run.relay.requests_until(REATTACH, |requests| {
        [permission_path, &snapshot_path]
            .iter()
            .all(|path| requests.iter().any(|line| line.starts_with(path)))
    });
    for path in [permission_path, &snapshot_path] {
        assert!(
            fresh.iter().any(|line| line.starts_with(path)),
            "fresh native recovery never read {path}: {fresh:?}"
        );
    }
    eprintln!("fresh native reconnect: {fresh:?}");
    let permission = fresh
        .iter()
        .position(|line| line.starts_with(permission_path));
    let snapshot = fresh
        .iter()
        .position(|line| line.starts_with(&snapshot_path));
    assert!(
        permission
            .zip(snapshot)
            .is_some_and(|(permission, snapshot)| permission < snapshot),
        "fresh native recovery omitted or reordered its baseline: {fresh:?}"
    );
    assert_eq!(
        settle_submits(
            run.address,
            &run.session,
            &run.submitted,
            Duration::from_secs(10)
        ),
        [1]
    );
}

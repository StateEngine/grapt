use super::*;

const W2_UNIT: &str = "secode-plugin-0-0-w2";
const W2_REAP: Duration = Duration::from_millis(300);
/// The single-number witness bound (CONTRACT.md): cleanup 100 ms +
/// reap 300 ms = 400 ms.
const W2_TOTAL: Duration = Duration::from_millis(400);

/// The survives-kill seam witness (PLR101 / PLR102): a child that never
/// reports an exit must run the full bound and land in the distinct
/// survives-kill outcome, named for the unit. The thread plus a 10 s
/// join bound turns an unbounded drain (PLR102) into a red instead of a
/// harness hang.
#[test]
fn a_child_that_never_reports_exit_yields_survives_kill_bounded_naming_the_unit() {
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let started = tokio::time::Instant::now();
        let deadline = started + W2_TOTAL;
        let outcome = post_kill_reap(&mut || Ok(false), deadline, W2_UNIT, W2_REAP);
        let _ = tx.send((outcome, started.elapsed()));
    });
    let (outcome, elapsed) = match rx.recv_timeout(Duration::from_secs(10)) {
        Ok(value) => value,
        Err(error) => panic!(
            "post_kill_reap did not return within the 10 s witness bound ({error}): the post-kill drain is unbounded (PLR102 shape)"
        ),
    };
    let Err(PluginError::SurvivesKill(message)) = outcome else {
        panic!(
            "the never-exiting child produced the wrong termination outcome: {outcome:?} after {elapsed:?}"
        );
    };
    assert!(
        message.contains(W2_UNIT),
        "the survives-kill outcome does not name its containment unit: {message}"
    );
    assert!(
        elapsed >= Duration::from_millis(350),
        "the reap bound did not run before the outcome: {elapsed:?}"
    );
    assert!(
        elapsed <= W2_TOTAL + Duration::from_millis(200),
        "the survives-kill outcome outlived the 400 ms witness bound plus 200 ms of poll-granularity slack: {elapsed:?}"
    );
}

/// A child that reports its exit on the third poll is reaped Ok before
/// the bound: the distinct outcome exists only at the bound, so an
/// immediate poll (the PLR101 shape) cannot masquerade as it.
#[test]
fn a_child_that_exits_on_the_third_poll_yields_ok_before_the_bound() {
    let polls = std::cell::Cell::new(0u32);
    let started = tokio::time::Instant::now();
    let deadline = started + W2_TOTAL;
    let outcome = post_kill_reap(
        &mut || {
            polls.set(polls.get() + 1);
            Ok(polls.get() >= 3)
        },
        deadline,
        W2_UNIT,
        W2_REAP,
    );
    let elapsed = started.elapsed();
    assert!(
        outcome.is_ok(),
        "the child that exited on the third poll was not reaped: {outcome:?}"
    );
    assert!(
        elapsed < Duration::from_millis(350),
        "the reap outlived the child's exit: {elapsed:?}"
    );
}

/// An unreadable status stays the distinct protocol outcome — it is
/// neither a reap nor a survives-kill.
#[test]
fn an_unreadable_status_yields_protocol() {
    let outcome = post_kill_reap(
        &mut || Err(std::io::Error::other("status unreadable")),
        tokio::time::Instant::now() + W2_TOTAL,
        W2_UNIT,
        W2_REAP,
    );
    let Err(PluginError::Protocol(message)) = outcome else {
        panic!("the unreadable status produced the wrong termination outcome: {outcome:?}");
    };
    assert!(
        message.contains("status unreadable"),
        "the protocol outcome lost the read error: {message}"
    );
}

/// The PLR105 witness: an exit that arrives within the observation
/// bound is classified as an exit. The probe must be polled past its
/// first non-exit answers, proving the observation loop ran rather
/// than the base single-read race, which would classify at the first
/// read and never see the exit.
#[test]
fn a_stream_end_observes_a_late_exit_within_the_bound_and_classifies_it_as_exited() {
    let polls = std::cell::Cell::new(0u32);
    let outcome = settle_stream_end_classification(
        None,
        &mut || {
            polls.set(polls.get() + 1);
            polls.get() >= 3
        },
        Duration::from_millis(400),
        None,
    );
    assert_eq!(
        outcome,
        StreamEnd::Exited("plugin bridge exited without replying".to_owned()),
        "an exit observed within the bound must be classified as Exited: {outcome:?}"
    );
    assert!(
        polls.get() >= 3,
        "the exit was not observed within the bound: {} polls",
        polls.get()
    );
}

/// No exit within the bound: the named still-running outcome, and the
/// bound actually ran (a single immediate read would return at 0 ms).
#[test]
fn a_stream_end_without_exit_within_the_bound_names_the_still_running_process() {
    let started = tokio::time::Instant::now();
    let outcome =
        settle_stream_end_classification(None, &mut || false, Duration::from_millis(200), None);
    let elapsed = started.elapsed();
    assert_eq!(
        outcome,
        StreamEnd::StillRunning(
            "plugin bridge closed its output while the process was still running".to_owned()
        )
    );
    assert!(
        elapsed >= Duration::from_millis(200),
        "the observation bound did not run: {elapsed:?}"
    );
    assert!(
        elapsed <= Duration::from_millis(500),
        "the still-running outcome outlived the bound plus 300 ms of poll-granularity slack: {elapsed:?}"
    );
}

/// A recorded limit event that explains the death keeps its own
/// reason at the observed exit.
#[test]
fn a_stream_end_keeps_a_recorded_resource_violation_reason() {
    let recorded = Violation {
        kind: ViolationKind::Resource,
        reason: "plugin bridge RSS reached its hard cgroup memory boundary".to_owned(),
    };
    let outcome = settle_stream_end_classification(
        Some(&recorded),
        &mut || true,
        Duration::from_millis(200),
        None,
    );
    assert_eq!(outcome, StreamEnd::Exited(recorded.reason.clone()));
}

/// A recorded non-limit violation (evidence loss, invalidation)
/// cannot rename the observed exit.
#[test]
fn a_stream_end_demotes_a_recorded_other_violation() {
    let recorded = Violation {
        kind: ViolationKind::Other,
        reason: "plugin containment OOM evidence was unavailable; the owned bridge was terminated fail-closed"
            .to_owned(),
    };
    let outcome = settle_stream_end_classification(
        Some(&recorded),
        &mut || true,
        Duration::from_millis(200),
        None,
    );
    assert_eq!(
        outcome,
        StreamEnd::Exited("plugin bridge exited without replying".to_owned())
    );
}

/// A stream error on a process that never reported an exit stays the
/// original corruption, verbatim.
#[test]
fn a_stream_end_with_a_stream_error_and_no_exit_keeps_the_corruption_verbatim() {
    let outcome = settle_stream_end_classification(
        None,
        &mut || false,
        Duration::from_millis(200),
        Some("plugin bridge closed its output inside a frame".to_owned()),
    );
    assert_eq!(
        outcome,
        StreamEnd::Corrupt("plugin bridge closed its output inside a frame".to_owned())
    );
}

/// A partial frame left by an already-exited process is an exit, not
/// a corruption: the observed exit outranks the stream error.
#[test]
fn a_stream_end_exited_outranks_the_stream_error() {
    let outcome = settle_stream_end_classification(
        None,
        &mut || true,
        Duration::from_millis(200),
        Some("plugin bridge closed its output inside a frame".to_owned()),
    );
    assert_eq!(
        outcome,
        StreamEnd::Exited("plugin bridge exited without replying".to_owned())
    );
}

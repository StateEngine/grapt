use std::sync::Arc;

use grapt_types::DeliveryMode;

use super::super::{admit, seed, session, within};
use super::{
    allowance, build, fast_policy, open_inbox, stop_script, truncated_script, unstated_script,
    LocationSpec,
};
use crate::WakeOutcome;

// ---------------------------------------------------------------------------
// R2A: the provider's finish disposition routes the chain
// ---------------------------------------------------------------------------

/// TRUNCATED IS NOT IDLE.
///
/// The provider stopped at its output ceiling with the answer unfinished. The
/// pre-R2A router tested `finish == ToolCalls` and sent everything else to
/// `Idle`, so a truncated answer settled the drain as a clean, complete
/// success — the user got a cut-off answer and the system reported nothing
/// wrong. Continuation is not implemented (recorded as an R2A residual), so the
/// ruled behaviour until it is: refuse, loudly.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_truncated_stream_is_not_settled_as_a_complete_drain() {
    let inbox = open_inbox();
    let ses = session("truncated");
    seed(&inbox, &ses, "/work/truncated");
    admit(&inbox, &ses, "a", DeliveryMode::Queue);

    let (harness, _runtimes) = build(
        Arc::clone(&inbox),
        vec![(
            "/work/truncated",
            LocationSpec {
                scripts: vec![truncated_script()],
                ..LocationSpec::default()
            },
        )],
        fast_policy(1),
        allowance(2),
    );

    match within("wake", harness.coordinator.wake(ses.clone())).await {
        Err(error) => {
            let reported = format!("{error:?}");
            assert!(
                reported.contains("truncated"),
                "the refusal must name truncation so it is diagnosable, got {reported}"
            );
        }
        Ok(outcome) => panic!(
            "a truncated answer was reported as a successful drain ({outcome:?}); this is the \
             oracle defect the disposition type exists to prevent"
        ),
    }
}

/// THE CONTROL for the test above.
///
/// The same harness, the same single-Step shape, a stream that ends CLEANLY —
/// and the drain succeeds. Without this half, an implementation that refused
/// every finish would pass the truncation witness.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_clean_stop_still_settles_the_drain() {
    let inbox = open_inbox();
    let ses = session("cleanstop");
    seed(&inbox, &ses, "/work/clean");
    admit(&inbox, &ses, "a", DeliveryMode::Queue);

    let (harness, _runtimes) = build(
        Arc::clone(&inbox),
        vec![(
            "/work/clean",
            LocationSpec {
                scripts: vec![stop_script()],
                ..LocationSpec::default()
            },
        )],
        fast_policy(1),
        allowance(2),
    );

    match within("wake", harness.coordinator.wake(ses.clone())).await {
        Ok(WakeOutcome::Drained { promotions, .. }) => assert_eq!(promotions, 1),
        other => panic!("a clean stop must settle the drain, got {other:?}"),
    }
}

/// An unstated finish keeps the no-unsettled-work fallback rather than being
/// swept into the refusal path with truncation. The two are different
/// dispositions and route differently.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_unstated_finish_still_uses_the_no_unsettled_work_fallback() {
    let inbox = open_inbox();
    let ses = session("unstated");
    seed(&inbox, &ses, "/work/unstated");
    admit(&inbox, &ses, "a", DeliveryMode::Queue);

    let (harness, _runtimes) = build(
        Arc::clone(&inbox),
        vec![(
            "/work/unstated",
            LocationSpec {
                scripts: vec![unstated_script()],
                ..LocationSpec::default()
            },
        )],
        fast_policy(1),
        allowance(2),
    );

    match within("wake", harness.coordinator.wake(ses.clone())).await {
        Ok(WakeOutcome::Drained { promotions, .. }) => assert_eq!(promotions, 1),
        other => panic!("an unstated finish must not be refused, got {other:?}"),
    }
}

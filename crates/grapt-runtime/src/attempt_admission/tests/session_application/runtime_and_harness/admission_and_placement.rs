use std::sync::atomic::Ordering;
use std::sync::Arc;

use grapt_types::DeliveryMode;

use super::super::{admit, event_count, locked, pending_count, seed, session, transcript, within};
use super::{allowance, build, fast_policy, open_inbox, ran, stop_script, LocationSpec};
use crate::{CoordinatorError, WakeOutcome};

// ---------------------------------------------------------------------------
// A) Admit and wake
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_durably_admitted_input_runs_once_and_leaves_one_transcript_row() {
    let inbox = open_inbox();
    let ses = session("admit");
    seed(&inbox, &ses, "/work/one");
    admit(&inbox, &ses, "a", DeliveryMode::Queue);

    let (harness, runtimes) = build(
        Arc::clone(&inbox),
        vec![(
            "/work/one",
            LocationSpec {
                scripts: vec![stop_script()],
                ..LocationSpec::default()
            },
        )],
        fast_policy(1),
        allowance(2),
    );

    let outcome = within("wake", harness.coordinator.wake(ses.clone())).await;
    match outcome {
        Ok(WakeOutcome::Drained {
            promotions,
            placement_lookups,
            ..
        }) => {
            assert_eq!(promotions, 1);
            assert_eq!(placement_lookups, 1, "placement resolves once per drain");
        }
        other => panic!("expected one promotion, got {other:?}"),
    }

    assert_eq!(ran(&runtimes[0]), vec!["msg_a".to_owned()]);
    assert_eq!(
        transcript(&harness.inbox, &ses),
        vec!["msg_a".to_owned()],
        "the admitted input becomes exactly one transcript row"
    );
    assert_eq!(
        event_count(&harness.inbox, &ses, "session.input.promoted.1"),
        1,
        "one promotion, one durable promoted event"
    );
    assert_eq!(pending_count(&harness.inbox, &ses), 0);
    assert!(
        event_count(&harness.inbox, &ses, "session.step.ended.1") > 0,
        "the Step wrote durable facts through the owner"
    );
}

// ---------------------------------------------------------------------------
// B) Placement: discovered from the seed, once per drain, and never leaked
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn each_session_runs_on_the_location_its_seed_names() {
    let inbox = open_inbox();
    let here = session("here");
    let there = session("there");
    seed(&inbox, &here, "/work/here");
    seed(&inbox, &there, "/work/there");
    admit(&inbox, &here, "h", DeliveryMode::Queue);
    admit(&inbox, &there, "t", DeliveryMode::Queue);

    let (harness, runtimes) = build(
        Arc::clone(&inbox),
        vec![
            (
                "/work/here",
                LocationSpec {
                    scripts: vec![stop_script()],
                    ..LocationSpec::default()
                },
            ),
            (
                "/work/there",
                LocationSpec {
                    scripts: vec![stop_script()],
                    ..LocationSpec::default()
                },
            ),
        ],
        fast_policy(1),
        allowance(2),
    );

    assert!(within("here", harness.coordinator.wake(here.clone()))
        .await
        .is_ok());
    assert!(within("there", harness.coordinator.wake(there.clone()))
        .await
        .is_ok());

    assert_eq!(ran(&runtimes[0]), vec!["msg_h".to_owned()]);
    assert_eq!(ran(&runtimes[1]), vec!["msg_t".to_owned()]);
    let crossed = locked(&runtimes[0].opened)
        .iter()
        .any(|(session, _)| session == there.as_str());
    assert!(!crossed, "work must not leak across Locations");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn admission_alone_never_boots_a_location() {
    // The lazy-boot law the oracle keeps (`core/src/session.ts:591`): admitting
    // a prompt must not start a Location's services. The inbox is
    // Session-scoped durable state in the one store, so admission never reaches
    // the service map at all.
    let inbox = open_inbox();
    let ses = session("lazyadmit");
    seed(&inbox, &ses, "/work/lazyadmit");

    let (harness, _) = build(
        Arc::clone(&inbox),
        vec![(
            "/work/lazyadmit",
            LocationSpec {
                scripts: vec![stop_script()],
                ..LocationSpec::default()
            },
        )],
        fast_policy(1),
        allowance(2),
    );

    admit(&inbox, &ses, "a", DeliveryMode::Queue);
    admit(&inbox, &ses, "b", DeliveryMode::Steer);
    assert_eq!(
        harness.map.boots.load(Ordering::SeqCst),
        0,
        "admission must boot nothing — only a drain may"
    );
    assert_eq!(pending_count(&harness.inbox, &ses), 2);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_drain_resolves_its_location_once_at_the_start_even_when_empty() {
    // `AGENTS.md`: placement is discovered "through SessionStore plus
    // LocationServiceMap.get(session.location) only when a drain starts". Once,
    // at the start — including a drain that turns out to have nothing to run,
    // because the drain is what owns the resolution, not the promotion.
    let inbox = open_inbox();
    let ses = session("lazy");
    seed(&inbox, &ses, "/work/lazy");

    let (harness, _) = build(
        Arc::clone(&inbox),
        vec![(
            "/work/lazy",
            LocationSpec {
                scripts: vec![stop_script()],
                ..LocationSpec::default()
            },
        )],
        fast_policy(1),
        allowance(2),
    );

    match within("wake", harness.coordinator.wake(ses.clone())).await {
        Ok(WakeOutcome::Drained {
            promotions,
            steps,
            placement_lookups,
            ..
        }) => {
            assert_eq!(promotions, 0);
            assert_eq!(steps, 0, "nothing eligible, so no Step");
            assert_eq!(placement_lookups, 1);
        }
        other => panic!("expected an empty drain, got {other:?}"),
    }
    assert_eq!(
        harness.map.boots.load(Ordering::SeqCst),
        1,
        "one drain, one resolution"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_multi_promotion_drain_resolves_one_location_and_runs_everything_on_it() {
    // The R3-d correction: resolving per promotion is not a cache question. A
    // drain that promoted four inputs could reach four runtimes, and two
    // resolutions of one drain's placement are two chances to disagree.
    let inbox = open_inbox();
    let ses = session("onceloc");
    seed(&inbox, &ses, "/work/onceloc");
    admit(&inbox, &ses, "s1", DeliveryMode::Steer);
    admit(&inbox, &ses, "s2", DeliveryMode::Steer);
    admit(&inbox, &ses, "q1", DeliveryMode::Queue);
    admit(&inbox, &ses, "q2", DeliveryMode::Queue);

    let (harness, runtimes) = build(
        Arc::clone(&inbox),
        vec![(
            "/work/onceloc",
            LocationSpec {
                scripts: (0..4).map(|_| stop_script()).collect(),
                ..LocationSpec::default()
            },
        )],
        fast_policy(1),
        allowance(2),
    );

    match within("wake", harness.coordinator.wake(ses.clone())).await {
        Ok(WakeOutcome::Drained {
            promotions,
            placement_lookups,
            ..
        }) => {
            assert_eq!(promotions, 4);
            assert_eq!(placement_lookups, 1);
        }
        other => panic!("expected four promotions, got {other:?}"),
    }

    assert_eq!(
        harness.map.boots.load(Ordering::SeqCst),
        1,
        "the service map is consulted ONCE for a four-promotion drain"
    );
    // And the one resolved runtime is the one that did all the work.
    assert_eq!(
        transcript(&harness.inbox, &ses).len(),
        4,
        "every input entered the transcript"
    );
    assert_eq!(
        locked(&runtimes[0].opened).len(),
        3,
        "three chains — the steer batch shares one — all on the same runtime"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_session_whose_location_cannot_be_served_fails_as_a_placement_fault() {
    let inbox = open_inbox();
    let ses = session("noloc");
    seed(&inbox, &ses, "/work/unregistered");
    admit(&inbox, &ses, "x", DeliveryMode::Queue);

    // No Location is registered for the seeded directory.
    let (harness, _) = build(Arc::clone(&inbox), Vec::new(), fast_policy(1), allowance(2));

    let outcome = within("wake", harness.coordinator.wake(ses.clone())).await;
    assert!(
        matches!(outcome, Err(CoordinatorError::Placement { .. })),
        "an unservable Location is a placement fault: {outcome:?}"
    );
    assert!(!harness.coordinator.is_locally_draining(&ses));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_unseeded_session_is_a_typed_rejection_not_an_empty_drain() {
    let inbox = open_inbox();
    let ghost = session("ghost");
    let (harness, _) = build(Arc::clone(&inbox), Vec::new(), fast_policy(1), allowance(2));

    assert!(matches!(
        within("wake", harness.coordinator.wake(ghost.clone())).await,
        Err(CoordinatorError::UnknownSession { .. })
    ));
}

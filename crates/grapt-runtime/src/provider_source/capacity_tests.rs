//! A deadline may win without polling the reservation future even once.
use super::*;

#[path = "local_capacity_tests.rs"]
mod local_capacity;

fn source(label: &str) -> Harness {
    let opener = Arc::new(AttemptOpener::deterministic(
        [transcript_script()],
        route(),
        credential(),
    ));
    harness(
        label,
        opener,
        Box::new(RegistryPlan {
            registry: ModelRegistry::install(catalog("capacity-prepoll")),
        }),
    )
}

#[test]
fn a_wait_dropped_before_its_first_poll_cannot_stage_an_unreserved_attempt() {
    let mut harness = source("ses_capacity_unpolled");
    let ids = Arc::new(SeqAttemptIds(AtomicUsize::new(0)));
    harness.source.ids = ids.clone();
    drop(harness.source.reserve_capacity());
    let preparation = harness
        .source
        .prepare(harness.step, 1, crate::HistoryGeneration::new(0));
    let (_handle, cancellation) = step_interrupt();
    let prepared = crate::attempt_admission::AttemptProtocol::prepare(
        preparation,
        Arc::new(NoTools),
        RecordingOwner::new(),
        cancellation.residency(),
        cancellation.signal(),
    );
    match prepared {
        Err((_, refusal)) => {
            assert!(matches!(refusal, AttemptAdmissionRefusal::Saturated { .. }));
            let message = refusal.to_string();
            assert!(
                message.contains("retryable") && message.contains("followup_task"),
                "{message}"
            );
        }
        Ok(_) => panic!("an unpolled, discarded capacity wait staged an unreserved attempt"),
    }
    assert_eq!(
        ids.0.load(Ordering::SeqCst),
        0,
        "a timed-out wait minted an attempt"
    );
    assert_eq!(receipt_row(&harness.inbox, &attempt("pat_u2_1")), None);
    assert_eq!(harness.source.ledger().held(), 0);
}

#[tokio::test]
async fn an_acquired_reservation_clears_the_prior_discarded_wait_refusal() {
    let mut harness = source("ses_capacity_acquired");
    drop(harness.source.reserve_capacity());
    harness.source.reserve_capacity().await;
    let preparation = harness
        .source
        .prepare(harness.step, 1, crate::HistoryGeneration::new(0));
    let (_handle, cancellation) = step_interrupt();
    assert!(
        crate::attempt_admission::AttemptProtocol::prepare(
            preparation,
            Arc::new(NoTools),
            RecordingOwner::new(),
            cancellation.residency(),
            cancellation.signal(),
        )
        .is_ok(),
        "a completed reservation retained the cancelled wait's refusal"
    );
    assert_eq!(harness.source.ledger().held(), 1);
}

#[tokio::test(start_paused = true)]
async fn a_wedged_holder_expires_retryably_without_minting_an_attempt() {
    let limits = grapt_provider::CodexLimits::new(
        std::num::NonZeroUsize::new(1).unwrap_or_else(|| panic!("nonzero capacity")),
    );
    let deadlines = CodexDeadlines::new(Duration::from_secs(1), Duration::from_secs(1))
        .unwrap_or_else(|error| panic!("deadlines: {error}"));
    let opener = Arc::new(
        AttemptOpener::codex(
            deadlines,
            limits,
            "https://127.0.0.1:1",
            route(),
            "capacity-test",
            credential(),
        )
        .unwrap_or_else(|error| panic!("opener: {error:?}")),
    );
    let _wedged = opener
        .reserve_capacity()
        .await
        .unwrap_or_else(|error| panic!("held slot: {error}"));
    let mut harness = harness(
        "ses_capacity_wedged",
        opener,
        Box::new(RegistryPlan {
            registry: ModelRegistry::install(catalog("capacity-wedged")),
        }),
    );
    let states = Arc::new(Mutex::new(Vec::new()));
    let observed = states.clone();
    harness.source.capacity_observer = Some(Arc::new(move |position| {
        observed
            .lock()
            .unwrap_or_else(|_| panic!("observations"))
            .push(position);
    }));
    let ids = Arc::new(SeqAttemptIds(AtomicUsize::new(0)));
    harness.source.ids = ids.clone();
    let progress = harness.source.capacity_progress();
    assert!(crate::step::capacity::until_stalled(
        harness.source.reserve_capacity(),
        progress,
        Duration::from_secs(10)
    )
    .await
    .is_none());
    assert_eq!(
        *states.lock().unwrap_or_else(|_| panic!("observations")),
        [Some(1), None]
    );
    let preparation = harness
        .source
        .prepare(harness.step, 1, crate::HistoryGeneration::new(0));
    let (_handle, cancellation) = step_interrupt();
    match crate::attempt_admission::AttemptProtocol::prepare(
        preparation,
        Arc::new(NoTools),
        RecordingOwner::new(),
        cancellation.residency(),
        cancellation.signal(),
    ) {
        Err((_, AttemptAdmissionRefusal::Saturated { detail })) => {
            assert!(detail.contains("wait"));
        }
        _ => panic!("a stalled holder did not produce the retryable capacity refusal"),
    }
    assert_eq!(ids.0.load(Ordering::SeqCst), 0);
    assert_eq!(harness.source.ledger().held(), 0);
}

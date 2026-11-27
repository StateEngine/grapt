use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use grapt_provider::DeterministicAdapter;
use grapt_types::DeliveryMode;

use super::super::{
    admit, event_count, locked, pending_count, seed, session, transcript, within, FixedClock, Gate,
    ReaderCensus, TestIds,
};
use super::{
    allowance, build, fast_policy, open_inbox, ran, stop_script, tool_script, AttemptLog,
    LocationSpec, RecordingTools, TestLocation, TestMap,
};
use crate::{
    Boundary, CoordinatorError, Promotion, RuntimeApplication, SessionApplication, WakeOutcome,
};

// ---------------------------------------------------------------------------
// G2) A steer admitted mid-chain promotes at the next Step boundary
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_steer_admitted_mid_chain_reaches_the_next_request_before_queued_work_moves() {
    // The delivery law's hard case: a prompt typed while the model is calling
    // tools must reach the NEXT provider request, and an explicitly queued
    // input must not jump into that same boundary. The cut is exact — the
    // steer is admitted while the first attempt's reader is blocked, so it
    // provably lands mid-chain rather than before the drain started.
    let inbox = open_inbox();
    let ses = session("midsteer");
    seed(&inbox, &ses, "/work/midsteer");
    admit(&inbox, &ses, "a", DeliveryMode::Queue);

    let gate = Gate::new();
    let (harness, runtimes) = build(
        Arc::clone(&inbox),
        vec![(
            "/work/midsteer",
            LocationSpec {
                // Step 1 calls a tool; step 2 is the continuation; step 3
                // answers the queued input on its own chain.
                scripts: vec![tool_script("call_1", "read"), stop_script(), stop_script()],
                gate: Some(gate.clone()),
                ..LocationSpec::default()
            },
        )],
        fast_policy(1),
        allowance(4),
    );

    let drain = {
        let coordinator = Arc::clone(&harness.coordinator);
        let ses = ses.clone();
        tokio::spawn(async move { coordinator.wake(ses).await })
    };

    // Exact cut: step 1's reader is open, so the chain is genuinely mid-flight.
    gate.wait().await;
    admit(&inbox, &ses, "mid", DeliveryMode::Steer);
    admit(&inbox, &ses, "later", DeliveryMode::Queue);
    // Release this attempt and every later one. Permits cannot be lost, so this
    // is a release, not a race.
    for _ in 0..8 {
        gate.open();
    }

    let outcome = match within("drain", drain).await {
        Ok(Ok(outcome)) => outcome,
        other => panic!("drain failed: {other:?}"),
    };
    match outcome {
        WakeOutcome::Drained {
            promotions, steps, ..
        } => {
            assert_eq!(promotions, 3, "the input, the mid-chain steer, the queue");
            assert_eq!(steps, 3, "step, continuation, and the queued input's step");
        }
        other => panic!("expected a three-Step drain, got {other:?}"),
    }

    let contexts = locked(&runtimes[0].log).contexts.clone();
    assert_eq!(contexts.len(), 3, "three attempts: {contexts:?}");
    assert_eq!(
        contexts[0],
        vec!["msg_a".to_owned()],
        "the first request predates the steer"
    );
    assert!(
        contexts[1].contains(&"msg_mid".to_owned()),
        "THE LAW: the continuation's request carries the steer: {:?}",
        contexts[1]
    );
    assert!(
        !contexts[1].contains(&"msg_later".to_owned()),
        "queued work must not enter a boundary that owes continuation: {:?}",
        contexts[1]
    );

    assert_eq!(
        ran(&runtimes[0]),
        vec!["msg_a".to_owned(), "msg_later".to_owned()],
        "the steer joined the chain in flight; only the queued input opened a new one"
    );
    assert_eq!(
        transcript(&harness.inbox, &ses),
        vec![
            "msg_a".to_owned(),
            "msg_mid".to_owned(),
            "msg_later".to_owned()
        ],
        "steer before queue, in durable order"
    );
    assert_eq!(pending_count(&harness.inbox, &ses), 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_promoted_batch_resets_the_step_allowance_exactly_once() {
    // `AGENTS.md`: "Promoting any new user input resets the selected agent's
    // step allowance; a batch of steers resets it once."
    //
    // The allowance is 2. Steps 1-2 spend it. A batch of TWO steers then
    // promotes at that boundary — if the reset were per steer the chain would
    // get four more Steps; because the batch resets once it gets exactly two,
    // and the fifth Step is refused.
    let inbox = open_inbox();
    let ses = session("reset");
    seed(&inbox, &ses, "/work/reset");
    admit(&inbox, &ses, "a", DeliveryMode::Queue);

    let gate = Gate::new();
    let (harness, runtimes) = build(
        Arc::clone(&inbox),
        vec![(
            "/work/reset",
            LocationSpec {
                scripts: vec![
                    tool_script("call_1", "read"),
                    tool_script("call_2", "read"),
                    tool_script("call_3", "read"),
                    tool_script("call_4", "read"),
                    stop_script(),
                ],
                gate: Some(gate.clone()),
                ..LocationSpec::default()
            },
        )],
        fast_policy(1),
        allowance(2),
    );

    let drain = {
        let coordinator = Arc::clone(&harness.coordinator);
        let ses = ses.clone();
        tokio::spawn(async move { coordinator.wake(ses).await })
    };

    gate.wait().await; // step 1's reader
    gate.open();
    gate.wait().await; // step 2's reader — the allowance is about to be spent
    admit(&inbox, &ses, "m1", DeliveryMode::Steer);
    admit(&inbox, &ses, "m2", DeliveryMode::Steer);
    for _ in 0..8 {
        gate.open();
    }

    let woke = match within("drain", drain).await {
        Ok(woke) => woke,
        Err(error) => panic!("the drain task must finish: {error}"),
    };
    match woke {
        Err(CoordinatorError::StepBudgetExhausted {
            session,
            policy,
            spent,
            granted,
        }) => {
            assert_eq!(session, ses, "the exhausted budget names its Session");
            assert_eq!(policy.as_str(), "step-allowance-v1");
            assert_eq!(spent, 2, "the exhausted budget names spent Steps");
            assert_eq!(granted, 2, "the exhausted budget names the batch grant");
        }
        other => panic!("expected the allowance to refuse a fifth Step, got {other:?}"),
    }

    // Two Steps before the batch, two after it: the reset happened once.
    let steps: Vec<u32> = locked(&runtimes[0].log)
        .opened
        .iter()
        .map(|(step, _, _)| *step)
        .collect();
    assert_eq!(
        steps,
        vec![1, 2, 3, 4],
        "the batch bought exactly two more Steps, not two per steer"
    );
    assert_eq!(
        transcript(&harness.inbox, &ses),
        vec!["msg_a".to_owned(), "msg_m1".to_owned(), "msg_m2".to_owned()],
    );
}

// ---------------------------------------------------------------------------
// H) No duplicate transcript under racing wakes
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn racing_wakes_promote_every_input_exactly_once() {
    // The coordinator guarantees one drain per Session; the inbox guarantees
    // one promotion per input. Together they must produce exactly one
    // transcript row and one run per admitted input, however the wakes race.
    const INPUTS: usize = 24;

    let inbox = open_inbox();
    let ses = session("race");
    seed(&inbox, &ses, "/work/race");

    let (harness, runtimes) = build(
        Arc::clone(&inbox),
        vec![(
            "/work/race",
            LocationSpec {
                scripts: (0..INPUTS).map(|_| stop_script()).collect(),
                ..LocationSpec::default()
            },
        )],
        fast_policy(1),
        allowance(2),
    );

    let mut handles = Vec::new();
    for index in 0..INPUTS {
        admit(&inbox, &ses, &format!("r{index}"), DeliveryMode::Steer);
        let coordinator = Arc::clone(&harness.coordinator);
        let ses = ses.clone();
        handles.push(tokio::spawn(async move { coordinator.wake(ses).await }));
    }
    for (index, handle) in handles.into_iter().enumerate() {
        let result = within(&format!("wake {index}"), handle).await;
        assert!(matches!(result, Ok(Ok(_))), "wake {index}: {result:?}");
    }
    // Drain anything admitted after the last drain released.
    let _ = within("final", harness.coordinator.wake(ses.clone())).await;

    let transcript = transcript(&harness.inbox, &ses);
    assert_eq!(
        transcript.len(),
        INPUTS,
        "every input must appear exactly once: {transcript:?}"
    );
    let mut deduped = transcript.clone();
    deduped.sort();
    deduped.dedup();
    assert_eq!(deduped.len(), INPUTS, "no duplicate transcript rows");
    assert_eq!(
        event_count(&harness.inbox, &ses, "session.input.promoted.1"),
        INPUTS as i64,
        "one promoted event per input"
    );
    assert_eq!(pending_count(&harness.inbox, &ses), 0);

    // How many CHAINS the Location opened depends on how the racing wakes fell
    // into batches, and that is not a law — a steer that joins a chain in
    // flight is correct. What is a law: no input ever opens a chain twice, and
    // every chain answers an input that was really admitted.
    let opened = ran(&runtimes[0]);
    let mut deduped_chains = opened.clone();
    deduped_chains.sort();
    deduped_chains.dedup();
    assert_eq!(
        deduped_chains.len(),
        opened.len(),
        "an input opened a chain twice: {opened:?}"
    );
    assert!(
        opened.iter().all(|input| transcript.contains(input)),
        "a chain ran for an input that never entered the transcript: {opened:?}"
    );
    assert!(
        !opened.is_empty() && opened.len() <= INPUTS,
        "chains opened must be between one and one-per-input: {}",
        opened.len()
    );
    // Every input was answered by SOME chain: the last attempt's request
    // carried the whole transcript, so nothing was promoted and then stranded.
    let contexts = locked(&runtimes[0].log).contexts.clone();
    let last = match contexts.last() {
        Some(last) => last.clone(),
        None => panic!("the drain must have opened at least one attempt"),
    };
    assert_eq!(
        last.len(),
        INPUTS,
        "the final request must carry every promoted input: {last:?}"
    );
}

// ---------------------------------------------------------------------------
// I) The seam's own contracts, exercised directly
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_eligibility_pass_promotes_at_most_one_input() {
    // The unit the drain needs: promoting a whole boundary up front would put
    // inputs into transcript history that an interrupt then guarantees never
    // run.
    let inbox = open_inbox();
    let ses = session("single");
    seed(&inbox, &ses, "/work/single");
    admit(&inbox, &ses, "s1", DeliveryMode::Steer);
    admit(&inbox, &ses, "s2", DeliveryMode::Steer);

    let ids = Arc::new(TestIds::default());
    let map = Arc::new(TestMap::default());
    let application = RuntimeApplication::new_internal(
        Arc::clone(&inbox),
        map,
        ids,
        Arc::new(FixedClock),
        fast_policy(1),
        Some(allowance(2)),
    );

    // No Location is registered, so beginning a drain fails at the ONE
    // placement seam — which is itself the proof that promotion is not reached
    // through the Location: the failure is Placement, never a silent empty
    // inbox.
    match application.begin_drain(&ses) {
        Err(CoordinatorError::Placement { .. }) => {}
        other => panic!("expected a placement fault, got {other:?}"),
    }

    let registered = Arc::new(TestMap::default());
    let application = RuntimeApplication::new_internal(
        Arc::clone(&inbox),
        Arc::clone(&registered),
        Arc::new(TestIds::default()),
        Arc::new(FixedClock),
        fast_policy(1),
        Some(allowance(2)),
    );
    registered.register(Arc::new(TestLocation {
        location: match grapt_types::LocationRef::new("/work/single", None) {
            Ok(reference) => reference.canonical_id(),
            Err(error) => panic!("witness directory must be valid: {error}"),
        },
        inbox: Arc::clone(&inbox),
        ids: Arc::new(TestIds::default()),
        adapter: Arc::new(Mutex::new(DeterministicAdapter::new(vec![stop_script()]))),
        log: Arc::new(Mutex::new(AttemptLog::default())),
        tools: Arc::new(RecordingTools::default()),
        reloads: Arc::new(AtomicUsize::new(0)),
        opened: Arc::new(Mutex::new(Vec::new())),
        chains: AtomicU64::new(0),
        gate: None,
        readers: Arc::new(ReaderCensus::default()),
        close_gate: None,
        refuse_commits_after: None,
        refuse_chain: false,
        record_only: false,
        step_limit: None,
        notifications: Arc::new(Mutex::new(Vec::new())),
    }));

    let mut drain = match application.begin_drain(&ses) {
        Ok(drain) => drain,
        Err(error) => panic!("drain must begin: {error}"),
    };
    assert!(
        !application.continuation_owed(&drain),
        "a drain owes nothing before it promotes"
    );

    match application.next_eligible(&mut drain, Boundary::Steer) {
        Ok(Some(Promotion { input, delivery })) => {
            assert_eq!(input.message().as_str(), "msg_s1");
            assert_eq!(delivery, DeliveryMode::Steer);
        }
        other => panic!("expected exactly one steer, got {other:?}"),
    }
    assert_eq!(
        pending_count(&inbox, &ses),
        1,
        "the second steer is still pending after one pass"
    );
    assert_eq!(transcript(&inbox, &ses), vec!["msg_s1".to_owned()]);
    assert!(
        application.continuation_owed(&drain),
        "a promotion that joined the drain owes a Step"
    );

    // The second pass of the SAME batch promotes the second steer into the
    // chain the first one opened — one input per pass, one chain for the batch.
    match application.next_eligible(&mut drain, Boundary::Steer) {
        Ok(Some(Promotion { input, .. })) => assert_eq!(input.message().as_str(), "msg_s2"),
        other => panic!("expected the second steer, got {other:?}"),
    }
    assert_eq!(pending_count(&inbox, &ses), 0);
    assert_eq!(
        locked(&registered_runtime(&registered).opened).len(),
        1,
        "the batch opened ONE chain"
    );
    assert!(matches!(
        application.next_eligible(&mut drain, Boundary::Steer),
        Ok(None)
    ));
}

/// The single runtime a witness registered, for assertions about it.
fn registered_runtime(map: &Arc<TestMap>) -> Arc<TestLocation> {
    let runtimes = locked(&map.runtimes);
    match runtimes.values().next() {
        Some(runtime) => Arc::clone(runtime),
        None => panic!("a runtime must be registered"),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn placement_is_re_read_from_the_store_at_every_drain_start() {
    // SCOPE NOTE — across-drain placement MOVEMENT is deliberately deferred,
    // with evidence rather than assumption. Measured over the frozen clone
    // (the 2026-08-08 sanitized clone, 69,740 events / 305
    // sessions): no aggregate carries a second `session.created.1` (0 of 305),
    // and no event type other than the seed carries a placement field — the
    // only non-seed payloads matching `directory`/`workspaceID` are tool
    // OUTPUT text (`session.tool.success.*`, `session.tool.called.1`,
    // `session.reasoning.ended.1`). The 284 sessions whose projected
    // `session.directory` differs from their seed differ ONLY by separator
    // canonicalization (`C:\Users\example` vs `C:/Users/example`), which is what
    // `LocationRef` already does; workspace drift is 0 of 305.
    //
    // So there is no durable movement fact to fold, and inventing one would be
    // fiction. What this witness pins instead is the seam where such a fact
    // WOULD take effect: placement is read from the store at every drain start,
    // never cached across drains, so a future move event changes where the next
    // drain runs without any change here.
    let inbox = open_inbox();
    let ses = session("reread");
    seed(&inbox, &ses, "/work/reread");

    let (harness, _) = build(
        Arc::clone(&inbox),
        vec![(
            "/work/reread",
            LocationSpec {
                scripts: vec![stop_script(), stop_script()],
                ..LocationSpec::default()
            },
        )],
        fast_policy(1),
        allowance(2),
    );

    admit(&inbox, &ses, "a", DeliveryMode::Queue);
    assert!(within("first", harness.coordinator.wake(ses.clone()))
        .await
        .is_ok());
    admit(&inbox, &ses, "b", DeliveryMode::Queue);
    assert!(within("second", harness.coordinator.wake(ses.clone()))
        .await
        .is_ok());

    assert_eq!(
        harness.map.boots.load(Ordering::SeqCst),
        2,
        "each drain resolves placement for itself — one lookup each, never cached"
    );
    assert_eq!(
        transcript(&harness.inbox, &ses),
        vec!["msg_a".to_owned(), "msg_b".to_owned()]
    );
}

/// Windows-only by construction. The oracle's `canonicalKey` rewrites
/// separators on win32 and nowhere else, so on POSIX there is no rewrite for a
/// drain to resolve *through* — and a Windows-shaped seed is not a placement
/// there at all. The per-platform rule itself is witnessed in
/// `grapt-types::location`; what this adds is that the DRAIN resolves its
/// Location by that canonical key rather than by the raw seed string.
#[cfg(windows)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn placement_is_the_canonical_key_of_the_seeded_directory() {
    let inbox = open_inbox();
    let ses = session("canon");
    seed(&inbox, &ses, "C:\\work\\canon");

    let (harness, runtimes) = build(
        Arc::clone(&inbox),
        vec![(
            "C:/work/canon",
            LocationSpec {
                scripts: vec![stop_script()],
                ..LocationSpec::default()
            },
        )],
        fast_policy(1),
        allowance(2),
    );
    admit(&inbox, &ses, "a", DeliveryMode::Queue);

    assert!(within("wake", harness.coordinator.wake(ses.clone()))
        .await
        .is_ok());
    assert_eq!(ran(&runtimes[0]), vec!["msg_a".to_owned()]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_trailing_separator_names_a_different_location_than_the_bare_path() {
    // `canonicalKey` rewrites separators and normalizes nothing else, so these
    // are two Locations in the oracle. Trimming the separator here would make a
    // Session seeded at one directory run on a runtime registered for another —
    // and being *close* to the seeded directory is not the same as being it.
    let inbox = open_inbox();
    let ses = session("trail");
    seed(&inbox, &ses, "/work/trail/");

    let (harness, runtimes) = build(
        Arc::clone(&inbox),
        vec![(
            "/work/trail",
            LocationSpec {
                scripts: vec![stop_script()],
                ..LocationSpec::default()
            },
        )],
        fast_policy(1),
        allowance(2),
    );
    admit(&inbox, &ses, "a", DeliveryMode::Queue);

    match within("wake", harness.coordinator.wake(ses.clone())).await {
        Err(CoordinatorError::Placement { .. }) => {}
        other => panic!("the seeded Location must not resolve to the bare one: {other:?}"),
    }
    assert!(
        ran(&runtimes[0]).is_empty(),
        "and no work ran on the Location the seed did not name"
    );
}

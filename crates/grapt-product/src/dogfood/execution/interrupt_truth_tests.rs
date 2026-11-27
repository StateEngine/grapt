use std::sync::{Arc, Barrier};
use std::thread;

use grapt_runtime::InterruptOutcome;

use super::{ExecutionRefusal, InterruptTruth};
use grapt_types::SessionId;

fn session(value: &str) -> SessionId {
    match value.parse() {
        Ok(session) => session,
        Err(_) => panic!("test SessionId is valid"),
    }
}

/// An opt-in cap (`--max-steps` or an agent's `steps`) is the only Step
/// budget a coding chain can exhaust.
const CAP_POLICY: &str = "max-steps-v1";

fn sustained_failure() -> grapt_session::execution::Failure {
    grapt_session::execution::Failure::StepBudgetExhausted {
        policy: grapt_types::PolicyVersion::new(CAP_POLICY)
            .unwrap_or_else(|| panic!("Step budget policy refused")),
        spent: 8,
        granted: 8,
    }
}

#[test]
fn coding_chain_has_no_turn_budget() {
    assert!(
        super::coding_step_allowance().is_none(),
        "the coding chain gained a turn budget without an explicit product decision"
    );
}

#[test]
fn swr_b02_typed_wake_failures_keep_their_public_class() {
    use grapt_runtime::CoordinatorError;
    use grapt_session::execution::Failure;

    let session = session("ses_swr_public_failure");
    let budget = CoordinatorError::StepBudgetExhausted {
        session: session.clone(),
        policy: grapt_types::PolicyVersion::new(CAP_POLICY)
            .unwrap_or_else(|| panic!("Step budget policy refused")),
        spent: 8,
        granted: 8,
    };
    assert_eq!(
        super::observations::public_failure(&budget),
        sustained_failure()
    );
    let context = CoordinatorError::Context {
        session: session.clone(),
        failure: grapt_types::ContextFailure::CompactionNoProgress,
        continuity: None,
        reason: "step 3 failed: private runtime detail".to_owned(),
    };
    assert_eq!(
        super::observations::public_failure(&context),
        Failure::ContextLimit
    );
    let other = CoordinatorError::Run {
        session: session.as_str().to_owned(),
        reason: "provider said something private".to_owned(),
    };
    assert_eq!(
        super::observations::public_failure(&other),
        Failure::Generic
    );
    for failure in [sustained_failure(), Failure::ContextLimit, Failure::Generic] {
        let public = failure.public();
        assert!(!public.message.contains("private"), "{public:?}");
    }
    assert!(sustained_failure()
        .public()
        .message
        .contains("8 of 8 Steps"));
}

#[test]
fn native_continuity_failures_publish_closed_recovery_without_runtime_text() {
    use grapt_session::execution::ContinuityFailure;
    for cause in [
        ContinuityFailure::Capability,
        ContinuityFailure::Principal,
        ContinuityFailure::Incomplete,
        ContinuityFailure::Transport,
        ContinuityFailure::Rebound,
        ContinuityFailure::TimedOut { seconds: 7 },
    ] {
        let error = grapt_runtime::CoordinatorError::Context {
            session: session("ses_native_recovery"),
            failure: grapt_types::ContextFailure::ContinuityUnavailable,
            continuity: Some(cause),
            reason: "private-capability private-cursor hostile remote explanation".into(),
        };
        let public = super::observations::public_failure(&error).public();
        assert_eq!(public.name, "ContextLimit");
        assert_eq!(public.message, cause.message());
        assert!(!public.message.contains("private"));
        assert!(!public.message.contains("hostile"));
    }
    let unrelated = grapt_runtime::CoordinatorError::Context {
        session: session("ses_native_unrelated"),
        failure: grapt_types::ContextFailure::CompactionNoProgress,
        continuity: Some(ContinuityFailure::Capability),
        reason: "private runtime text".into(),
    };
    assert_eq!(
        super::observations::public_failure(&unrelated),
        grapt_session::execution::Failure::ContextLimit,
        "a diagnostic cannot relabel the actual context class"
    );
}

#[test]
fn coordinator_noop_and_error_never_leak_interrupted_terminal_truth() {
    let truth = InterruptTruth::default();
    let session = session("ses_interrupt_noop");

    let no_op = truth.observe(&session, || Ok(InterruptOutcome::NoOpIdle));
    assert_eq!(no_op.ok(), Some(InterruptOutcome::NoOpIdle));
    assert!(
        !truth.take(&session),
        "post-run/pre-active-map NoOpIdle leaked an interrupted terminal"
    );

    let failed = truth.observe(&session, || {
        Err(ExecutionRefusal::Run("coordinator refused".to_owned()))
    });
    assert!(failed.is_err(), "coordinator refusal became success");
    assert!(
        !truth.take(&session),
        "coordinator error leaked an interrupted terminal into a later run"
    );
}

#[test]
fn interrupted_outcome_is_recorded_before_terminal_publication_can_read_it() {
    let truth = Arc::new(InterruptTruth::default());
    let session = session("ses_interrupt_order");
    let entered = Arc::new(Barrier::new(2));
    let terminal_attempted = Arc::new(Barrier::new(2));

    let observer_truth = Arc::clone(&truth);
    let observer_session = session.clone();
    let observer_entered = Arc::clone(&entered);
    let observer_terminal_attempted = Arc::clone(&terminal_attempted);
    let observer = thread::spawn(move || {
        observer_truth.observe(&observer_session, || {
            let _ = observer_entered.wait();
            let _ = observer_terminal_attempted.wait();
            Ok(InterruptOutcome::Interrupted)
        })
    });

    let _ = entered.wait();
    let terminal_truth = Arc::clone(&truth);
    let terminal_session = session.clone();
    let terminal = thread::spawn(move || {
        terminal_truth.take_after_observed_attempt(&terminal_session, &terminal_attempted)
    });

    match observer.join() {
        Ok(Ok(InterruptOutcome::Interrupted)) => {}
        Ok(other) => panic!("coordinator outcome changed: {other:?}"),
        Err(_) => panic!("interrupt observer panicked"),
    }
    match terminal.join() {
        Ok((true, true)) => {}
        Ok((false, _)) => {
            panic!("terminal reader never blocked under interrupted outcome custody")
        }
        Ok((true, false)) => {
            panic!("terminal publication outran interrupted outcome custody")
        }
        Err(_) => panic!("terminal reader panicked"),
    }
}

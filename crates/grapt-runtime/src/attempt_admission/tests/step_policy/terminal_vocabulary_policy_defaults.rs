use super::{
    step, step_interrupt, test_version, Arc, AttemptTerminalReason, Duration, FakeHistory,
    InstantTools, InterruptionSource, PolicyVersion, RecordingOwner, ScriptSource, StepDriver,
    StepOutcome, StepPolicy, StepResult, StepTerminalReason, TerminalClass,
    RULED_DEFAULT_POLICY_VERSION,
};

// ---------------------------------------------------------------------------
// R2A: the split the `aborted` string could not express
// ---------------------------------------------------------------------------

/// THE SPLIT, at the attempt level.
///
/// A no-progress stall and a user interrupt both cancel the running attempt
/// through the same seam, and under the string carrier both settled as
/// `aborted`. Nothing downstream could tell a hung provider from someone
/// pressing stop. This asserts the stall's attempt terminal is a typed
/// `ProviderFailure { NoProgressTimeout }` AND is not the interruption.
#[tokio::test(start_paused = true)]
async fn a_watchdog_stall_is_not_an_interruption_at_the_attempt_level() {
    let mut source = ScriptSource::stalling(vec![Vec::new()]);
    let mut history = FakeHistory::new();
    let (_interrupt, cancel) = step_interrupt();
    let policy = match StepPolicy::new(
        test_version(),
        1,
        Duration::from_secs(2),
        1,
        Duration::from_secs(30),
    ) {
        Ok(policy) => policy,
        Err(error) => panic!("valid policy: {error}"),
    };

    let (_owner, report) = StepDriver::run(
        step(),
        policy,
        &mut source,
        &mut history,
        Arc::new(InstantTools),
        RecordingOwner::new(),
        cancel,
    )
    .await;

    let receipt = match report.receipts.first() {
        Some(receipt) => receipt,
        None => panic!("a stalled attempt must still leave a receipt"),
    };
    match &receipt.outcome {
        StepOutcome::Failed { failure } => {
            assert_eq!(
                failure.reason(),
                &AttemptTerminalReason::ProviderFailure {
                    class: TerminalClass::NoProgressTimeout
                },
                "the watchdog cancelled this attempt; no one interrupted it"
            );
            assert_ne!(
                failure.reason(),
                &AttemptTerminalReason::Interrupted {
                    by: InterruptionSource::User
                },
                "the oracle defect: a stall reported as a user interrupt"
            );
        }
        other => panic!("a stalled attempt must fail, got {other:?}"),
    }
}

/// THE SPLIT, at the attempt level, from the other side.
///
/// The control for the test above: the same seam, the same cancellation
/// machinery, a real interrupt — and a DIFFERENT typed reason. Without this
/// half, an implementation that wrote `NoProgressTimeout` for every
/// cancellation would pass.
#[tokio::test(start_paused = true)]
async fn an_interrupted_attempt_names_its_source() {
    let mut source = ScriptSource::stalling(vec![Vec::new()]);
    let mut history = FakeHistory::new();
    let (interrupt, cancel) = step_interrupt();
    let policy = match StepPolicy::new(
        test_version(),
        1,
        Duration::from_secs(2),
        1,
        // A window long enough that the watchdog cannot be what fires.
        Duration::from_secs(3_600),
    ) {
        Ok(policy) => policy,
        Err(error) => panic!("valid policy: {error}"),
    };

    let driver = StepDriver::run(
        step(),
        policy,
        &mut source,
        &mut history,
        Arc::new(InstantTools),
        RecordingOwner::new(),
        cancel,
    );
    let interrupting = async {
        tokio::time::sleep(Duration::from_millis(50)).await;
        interrupt.interrupt();
    };
    let ((_owner, report), ()) = tokio::join!(driver, interrupting);

    let receipt = match report.receipts.first() {
        Some(receipt) => receipt,
        None => panic!("an interrupted attempt must still leave a receipt"),
    };
    match &receipt.outcome {
        StepOutcome::Failed { failure } => assert_eq!(
            failure.reason(),
            &AttemptTerminalReason::Interrupted {
                by: InterruptionSource::User
            },
            "the interrupt's source is written where the interrupt happened"
        ),
        other => panic!("an interrupted attempt must fail, got {other:?}"),
    }

    // And it is carried up unflattened, not re-derived at the Step.
    match &report.result {
        StepResult::Failed { failure } => assert_eq!(
            failure.reason(),
            &StepTerminalReason::Interrupted {
                by: InterruptionSource::User
            }
        ),
        other => panic!("expected an interrupted Step, got {other:?}"),
    }
}

/// `PolicyLimit` names the policy that actually fired, not a constant.
///
/// Two runs, identical except for the policy's version identity. If the reason
/// carried a hard-coded or invented version — which is exactly what a `Default`
/// impl would have forced — both runs would report the same value and this
/// would fail on the second.
#[tokio::test(start_paused = true)]
async fn a_policy_limit_carries_the_version_of_the_policy_that_fired() {
    async fn exhaust(version: &str) -> StepTerminalReason {
        let named = match PolicyVersion::new(version) {
            Some(named) => named,
            None => panic!("a non-empty literal must build a PolicyVersion"),
        };
        let policy =
            match StepPolicy::new(named, 1, Duration::from_secs(2), 1, Duration::from_secs(30)) {
                Ok(policy) => policy,
                Err(error) => panic!("valid policy: {error}"),
            };
        let mut source = ScriptSource::stalling(vec![Vec::new()]);
        let mut history = FakeHistory::new();
        let (_interrupt, cancel) = step_interrupt();
        let (_owner, report) = StepDriver::run(
            step(),
            policy,
            &mut source,
            &mut history,
            Arc::new(InstantTools),
            RecordingOwner::new(),
            cancel,
        )
        .await;
        match report.result {
            StepResult::Failed { failure } => failure.reason().clone(),
            other => panic!("expected a policy stop, got {other:?}"),
        }
    }

    let first = exhaust("policy-under-test/alpha").await;
    let second = exhaust("policy-under-test/beta").await;

    match (&first, &second) {
        (
            StepTerminalReason::PolicyLimit { policy: one },
            StepTerminalReason::PolicyLimit { policy: two },
        ) => {
            assert_ne!(
                one, two,
                "the reason carried the same version for two different policies, so it is not \
                 reading the policy that fired"
            );
            assert_eq!(format!("{one}"), "policy-under-test/alpha");
            assert_eq!(format!("{two}"), "policy-under-test/beta");
        }
        other => panic!("both runs must stop on the attempt bound, got {other:?}"),
    }
}

/// The crate's own ruled defaults keep the oracle's numbers AND carry an
/// identity. Replaces `the_default_is_the_oracle_policy`, which could not check
/// the second half because there was nothing to check.
#[test]
fn the_ruled_default_policy_is_named_and_matches_the_oracle() {
    let policy = match StepPolicy::ruled_default() {
        Ok(policy) => policy,
        Err(error) => panic!("the crate's ruled default policy must be valid: {error}"),
    };
    assert_eq!(policy.max_attempts(), 5);
    assert_eq!(policy.max_overflow_rebuilds(), 1);
    assert_eq!(
        format!("{}", policy.version()),
        RULED_DEFAULT_POLICY_VERSION,
        "the ruled default must carry its own named identity"
    );
}

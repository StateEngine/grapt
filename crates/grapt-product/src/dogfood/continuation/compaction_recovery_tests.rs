use super::*;

fn fixture(label: &str) -> Result<(Session, DurableContinuation, TestContinuationScratch), String> {
    let (continuation, scratch) = DurableContinuation::empty_for_test(label)?;
    let recovery = Session::new(
        Registry::default(),
        continuation.session.clone(),
        continuation.input.clone(),
    );
    Ok((recovery, continuation, scratch))
}

#[test]
fn cr_recovery_classifies_typed_causes_without_error_text() {
    let detail = |class, classification| ProviderFailureDetail {
        class,
        classification,
    };
    assert_eq!(
        Cause::of(Some(&detail(ProviderFailureClass::Authentication, None))),
        Cause::Authentication
    );
    assert_eq!(
        Cause::of(Some(&detail(ProviderFailureClass::QuotaExceeded, None))),
        Cause::Quota
    );
    assert_eq!(
        Cause::of(Some(&detail(ProviderFailureClass::Transport, None))),
        Cause::Transient
    );
    assert_eq!(
        Cause::of(Some(&detail(
            ProviderFailureClass::InvalidRequest,
            Some(FailureClassification::ContextOverflow)
        ))),
        Cause::Context
    );
    // A 400 that names no overflow refuses one request shape; the cold summary
    // and the next input are real retries, so it must not pause as `Other`.
    assert_eq!(
        Cause::of(Some(&detail(ProviderFailureClass::InvalidRequest, None))),
        Cause::Transient
    );
    // Classes that name nothing retryable stay a permanent pause.
    assert_eq!(
        Cause::of(Some(&detail(ProviderFailureClass::ContentPolicy, None))),
        Cause::Other
    );
}

/// Every attempt terminal reason maps to a cause, including the one that must
/// not read as a permanent pause: a watchdog stall carries its class only in
/// the reason, and an interrupt says nothing against the compaction.
#[test]
fn cr_every_terminal_reason_maps_to_its_cause() {
    use grapt_types::{AttemptTerminalReason, ProviderFailureClass as Terminal};
    let cases = [
        (
            AttemptTerminalReason::ProviderFailure {
                class: Terminal::NoProgressTimeout,
            },
            Cause::Transient,
            "a stall retries on the next input",
        ),
        (
            AttemptTerminalReason::ProviderFailure {
                class: Terminal::Transport,
            },
            Cause::Transient,
            "transport is retry-eligible",
        ),
        (
            AttemptTerminalReason::ProviderFailure {
                class: Terminal::Authentication,
            },
            Cause::Authentication,
            "credentials wait for a change",
        ),
        (
            AttemptTerminalReason::ProviderFailure {
                class: Terminal::Quota,
            },
            Cause::Quota,
            "credit waits for a successful response",
        ),
        (
            AttemptTerminalReason::ProviderFailure {
                class: Terminal::InvalidRequest,
            },
            Cause::Transient,
            "a refused request shape retries on the next input",
        ),
        (
            AttemptTerminalReason::ContextFailure {
                failure: grapt_types::ContextFailure::Overflow,
            },
            Cause::Context,
            "overflow waits for budget or rewind",
        ),
        (
            AttemptTerminalReason::Interrupted {
                by: grapt_types::InterruptionSource::User,
            },
            Cause::Transient,
            "an interrupt says nothing against the compaction",
        ),
        (
            AttemptTerminalReason::ProviderEnded {
                completion: grapt_types::ProviderCompletionDisposition::AssertedComplete,
            },
            Cause::Other,
            "an ended attempt is not a retryable failure",
        ),
        (
            AttemptTerminalReason::DurabilityFailure,
            Cause::Other,
            "a refused write is not retried by compaction",
        ),
    ];
    for (reason, expected, why) in cases {
        assert_eq!(Cause::of_reason(&reason), expected, "{reason:?}: {why}");
    }
}

/// SC-REL-13: a summary the full adapter kept waiting past its bound is a
/// transient failure, not a permanent one; any other
/// refusal keeps whatever cause the call itself recorded.
#[test]
fn a_saturated_summary_refusal_is_transient() {
    assert_eq!(
        Cause::of_refusal(&grapt_runtime::SummaryRefusal::saturated(
            "the adapter already holds its capacity of 1 active attempts"
        )),
        Some(Cause::Transient)
    );
    assert_eq!(
        Cause::of_refusal(&"summary call is repeated or names stale history".into()),
        None
    );
}

#[test]
fn a_summary_that_left_no_usable_context_trims_only_the_next_input() -> Result<(), String> {
    for (cause, trims) in [
        (Cause::NoProgress, true),
        (Cause::DidNotFit, true),
        (Cause::Context, false),
    ] {
        let (mut recovery, _continuation, _scratch) = fixture("trim-next-input")?;
        recovery.failed(cause);
        assert!(!recovery.trim_requested(), "the same input is not trimmed");
        recovery.promote_input(
            &"msg_next_input"
                .parse()
                .map_err(|error: grapt_types::IdError| error.to_string())?,
        );
        assert_eq!(recovery.trim_requested(), trims, "{cause:?}");
        recovery.succeeded();
        assert!(!recovery.trim_requested());
    }
    Ok(())
}

#[test]
fn cr_recovery_public_failure_requires_a_report_and_is_consumed() -> Result<(), String> {
    let (recovery, _continuation, _scratch) = fixture("report-ownership")?;
    recovery.failed(Cause::Quota);
    // A failure nobody reported does not relabel the execution.
    assert_eq!(recovery.registry.take_failure(&recovery.session), None);
    recovery.report_failure();
    assert_eq!(
        recovery.registry.take_failure(&recovery.session),
        Some(Cause::Quota)
    );
    assert_eq!(recovery.registry.take_failure(&recovery.session), None);
    recovery.report_failure();
    // A later manual attempt replaces the observation without reporting an
    // automatic stop; stale advice cannot relabel its execution.
    recovery.failed(Cause::Authentication);
    assert_eq!(recovery.registry.take_failure(&recovery.session), None);
    Ok(())
}

/// A did-not-fit the runtime decided after a compaction that succeeded is
/// named so, and the next input runs trimmed.
#[test]
fn a_runtime_did_not_fit_names_itself_and_trims_the_next_input() -> Result<(), String> {
    let (mut recovery, _continuation, _scratch) = fixture("runtime-did-not-fit")?;
    assert_eq!(
        recovery.registry.did_not_fit(&recovery.session),
        Cause::DidNotFit
    );
    recovery.promote_input(
        &"msg_after_runtime_fit"
            .parse()
            .map_err(|error: grapt_types::IdError| error.to_string())?,
    );
    assert!(recovery.trim_requested());
    Ok(())
}

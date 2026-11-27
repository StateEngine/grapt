use super::*;

fn fixture(label: &str) -> Result<(Session, DurableContinuation, TestContinuationScratch), String> {
    let (continuation, scratch) = DurableContinuation::empty_for_test(label)?;
    let target = crate::dogfood::production::tests::projection_target()?;
    let recovery = Session::new(
        Registry::default(),
        continuation.session.clone(),
        continuation.input.clone(),
        &target,
        Some(128_000),
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
/// transient failure, not the permanent pause row A's child met; any other
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
fn cr_recovery_credential_change_clears_auth_but_not_credit() -> Result<(), String> {
    for cause in [Cause::Authentication, Cause::Quota] {
        let (recovery, continuation, _scratch) = fixture("credential-cause")?;
        recovery.failed(cause, &lock(&continuation.inbox))?;
        lock(&recovery.target).credential =
            CredentialVersion::new("new-version").ok_or("credential")?;
        let blocked = recovery.blocked(&lock(&continuation.inbox))?;
        assert_eq!(
            blocked,
            if cause == Cause::Authentication {
                None
            } else {
                Some(Cause::Quota)
            }
        );
    }
    Ok(())
}

#[test]
fn cr_recovery_context_budget_change_does_not_clear_an_auth_block() -> Result<(), String> {
    for cause in [Cause::Context, Cause::Authentication] {
        let (mut recovery, continuation, _scratch) = fixture("budget-cause")?;
        recovery.failed(cause, &lock(&continuation.inbox))?;
        recovery.budget = Some(256_000);
        let blocked = recovery.blocked(&lock(&continuation.inbox))?;
        assert_eq!(
            blocked,
            if cause == Cause::Context {
                None
            } else {
                Some(Cause::Authentication)
            }
        );
    }
    Ok(())
}

#[test]
fn cr_recovery_catalog_reload_does_not_clear_an_unchanged_target() -> Result<(), String> {
    for cause in [Cause::Context, Cause::Authentication, Cause::Quota] {
        let (recovery, continuation, _scratch) = fixture("model-account-cause")?;
        recovery.failed(cause, &lock(&continuation.inbox))?;
        let replacement = crate::dogfood::production::tests::projection_target()?;
        assert_eq!(lock(&recovery.target).model, *replacement.model_ref());
        assert!(!lock(&recovery.target)
            .model
            .is_same_issuance(replacement.model_ref()));
        lock(&recovery.target).model = replacement.model_ref().clone();
        assert_eq!(recovery.blocked(&lock(&continuation.inbox))?, Some(cause));
    }
    Ok(())
}

#[test]
fn cr_recovery_transient_requires_a_new_input_not_a_new_step() -> Result<(), String> {
    let (mut recovery, continuation, _scratch) = fixture("transient-input")?;
    recovery.failed(Cause::Transient, &lock(&continuation.inbox))?;
    assert_eq!(
        recovery.blocked(&lock(&continuation.inbox))?,
        Some(Cause::Transient)
    );
    recovery.input = "msg_next_input"
        .parse()
        .map_err(|error: grapt_types::IdError| error.to_string())?;
    assert_eq!(recovery.blocked(&lock(&continuation.inbox))?, None);
    Ok(())
}

#[test]
fn cr_recovery_capacity_refuses_without_evicting_another_block() -> Result<(), String> {
    let (recovery, continuation, _scratch) = fixture("capacity")?;
    recovery.failed(Cause::Quota, &lock(&continuation.inbox))?;
    let previous = lock(&recovery.registry.0)
        .get(&recovery.session)
        .cloned()
        .ok_or("block")?;
    {
        let mut sessions = lock(&recovery.registry.0);
        for index in 1..MAX_SUPPRESSED_SESSIONS {
            let session = format!("ses_blocked_{index}")
                .parse()
                .map_err(|error: grapt_types::IdError| error.to_string())?;
            sessions.insert(session, previous.clone());
        }
    }
    let mut another = recovery.clone();
    another.session = "ses_capacity_next"
        .parse()
        .map_err(|error: grapt_types::IdError| error.to_string())?;
    assert_eq!(
        another.blocked(&lock(&continuation.inbox))?,
        Some(Cause::Capacity)
    );
    assert_eq!(
        recovery.blocked(&lock(&continuation.inbox))?,
        Some(Cause::Quota)
    );
    assert_eq!(lock(&recovery.registry.0).len(), MAX_SUPPRESSED_SESSIONS);
    recovery.succeeded();
    assert_eq!(another.blocked(&lock(&continuation.inbox))?, None);
    Ok(())
}

#[test]
fn cr_recovery_public_failure_requires_a_report_and_is_consumed() -> Result<(), String> {
    let (recovery, continuation, _scratch) = fixture("report-ownership")?;
    recovery.failed(Cause::Quota, &lock(&continuation.inbox))?;
    assert_eq!(
        recovery.blocked(&lock(&continuation.inbox))?,
        Some(Cause::Quota)
    );
    // A threshold probe observes suppression but does not fail the execution.
    assert_eq!(recovery.registry.take_failure(&recovery.session), None);
    recovery.report_failure();
    assert_eq!(
        recovery.registry.take_failure(&recovery.session),
        Some(Cause::Quota)
    );
    assert_eq!(recovery.registry.take_failure(&recovery.session), None);
    recovery.report_failure();
    // A later manual attempt replaces policy observations without reporting
    // an automatic stop; stale recovery advice cannot relabel its execution.
    recovery.failed(Cause::Authentication, &lock(&continuation.inbox))?;
    assert_eq!(recovery.registry.take_failure(&recovery.session), None);
    Ok(())
}

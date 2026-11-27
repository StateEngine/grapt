//! Witnesses for the RESUMPTION seam.
//!
//! Split out of the owner's own witness file because the restart cut grew its
//! own vocabulary — reconciliation, continuation authority, and the four
//! refusals — and because the sibling file had reached the workspace's
//! god-file ceiling. Every helper is the SAME one the owner's witnesses use,
//! reached through `super::*`, so a fixture cannot drift between the two files.

use super::*;

/// A resumed Step continues its ordinals from DURABLE truth rather than
/// overwriting ordinal zero. This is the restart cut for content stamping.
///
/// The resumption modelled here is the ONLY one the contract admits: `pat_1`
/// delivered the Step's visible output and ended, `pat_2` opened and the
/// process was lost before `pat_2` emitted anything visible, and the restart
/// reconciles `pat_2`'s loss and authorizes `pat_3` to continue. An earlier
/// revision of this witness had `pat_1` write visible output and then had a
/// resumed owner append more content under `pat_2` with no reconciliation and
/// no authority at all — that is automatic continuation after visible output,
/// which is the provider resurrection the assignment forbids, asserted as the
/// law. See the refusal witnesses below for each combination it admitted.
#[test]
fn a_resumed_owner_continues_ordinals_from_durable_truth() {
    let scratch = Scratch::new("resume");
    let store = store();
    let clock = FrozenClock;
    let ids = CountingIds::default();
    let first_message;
    {
        let mut owner = match DurableAttemptOwner::elect(
            Arc::clone(&store),
            scratch.artifacts(),
            &clock,
            &ids,
            RecordingSink::default(),
            session(),
            step(1),
            None,
        ) {
            Ok(owner) => owner,
            Err(error) => panic!("the owner must elect: {error}"),
        };
        drive(
            &mut owner,
            vec![
                DurableFact::AttemptStarted {
                    attempt: attempt("pat_1"),
                },
                text_opened(0),
                text_delta(0, "before"),
                text_closed(0),
            ],
        );
        first_message = match owner.message() {
            Some(message) => message.clone(),
            None => panic!("content minted a message"),
        };
    }

    // The process "restarts": a new owner reads the next ordinal out of the
    // durable log, exactly as a caller would.
    let resume_at = history(&store).next_ordinal(&first_message);
    assert_eq!(resume_at, ContentOrdinal::new(1));
    // The runtime reconciled the loss BEFORE the owner exists: `pat_2` died
    // with no visible output of its own, the Step wrote no terminal, and
    // `pat_3` is explicitly authorized to continue.
    let resume = match ResumedMessage::new(
        first_message.clone(),
        resume_at,
        ProcessLossReconciliation::new(
            attempt("pat_2"),
            PriorAttempt::FailedBeforeVisibleOutput,
            false,
        ),
        ContinuationAuthority::grant(attempt("pat_2"), attempt("pat_3")),
    ) {
        Ok(resume) => resume,
        Err(error) => panic!("a reconciled loss admits a continuation: {error}"),
    };
    let mut resumed = match DurableAttemptOwner::elect(
        Arc::clone(&store),
        scratch.artifacts(),
        &clock,
        &ids,
        RecordingSink::default(),
        session(),
        step(1),
        Some(resume),
    ) {
        Ok(owner) => owner,
        Err(error) => panic!("the resumed owner must elect: {error}"),
    };
    drive(
        &mut resumed,
        vec![
            DurableFact::AttemptStarted {
                attempt: attempt("pat_3"),
            },
            text_opened(0),
            text_delta(0, "after"),
            text_closed(0),
        ],
    );

    // ONE message, because the resumed owner adopted the identity the Step
    // already owned rather than minting a second one for the same Step. Its
    // item landed at ordinal ONE, so the sequence is still dense from zero and
    // the fold renders both items.
    let history = history(&store);
    let messages = match history.messages() {
        Ok(messages) => messages,
        Err(error) => panic!("the folded history must render: {error}"),
    };
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0].message, first_message);
    let content = match &messages[0].content {
        Some(content) => content,
        None => panic!("the resumed message has content"),
    };
    assert_eq!(content.len(), 2);
    assert_eq!(content.items()[0].item.payload().length(), 6);
    assert_eq!(content.items()[1].item.ordinal(), ContentOrdinal::new(1));
    assert_eq!(content.items()[1].item.payload().length(), 5);
}

/// The resumption a message identity and an ordinal alone used to admit.
///
/// Every one of these four is a DIFFERENT broken caller, and each was
/// constructible before the reconciliation and the authority became arguments.
/// The refusal lives in the constructor, so the owner never gets the chance to
/// resurrect: the combination is unnameable rather than merely refused later.
#[test]
fn a_resumption_the_contract_forbids_cannot_be_constructed() {
    let message = match "msg_df0a".parse::<MessageId>() {
        Ok(message) => message,
        Err(error) => panic!("msg_df0a is a valid message identity: {error}"),
    };
    let at = ContentOrdinal::new(1);
    let resume = |reconciliation, authority| {
        ResumedMessage::new(message.clone(), at, reconciliation, authority)
    };
    let granted = || ContinuationAuthority::grant(attempt("pat_2"), attempt("pat_3"));

    // A Step that already wrote its ONE terminal is finished. Continuing it
    // resurrects a STEP, not an attempt.
    match resume(
        ProcessLossReconciliation::new(
            attempt("pat_2"),
            PriorAttempt::FailedBeforeVisibleOutput,
            true,
        ),
        granted(),
    ) {
        Err(ResumeRefused::StepAlreadyTerminal) => {}
        other => panic!("a settled step has no continuation, got: {other:?}"),
    }

    // Visible output already reached the user under the lost attempt. A second
    // attempt appending to the same message silently extends what was shown.
    match resume(
        ProcessLossReconciliation::new(
            attempt("pat_2"),
            PriorAttempt::FailedAfterVisibleOutput,
            false,
        ),
        granted(),
    ) {
        Err(ResumeRefused::VisibleOutputAlreadyDelivered { attempt }) => {
            assert_eq!(attempt, "pat_2")
        }
        other => panic!("visible output forbids continuation, got: {other:?}"),
    }

    // Durable truth carries no terminal for the lost attempt at all. Ambiguous
    // is refused rather than treated as "probably failed".
    match resume(
        ProcessLossReconciliation::new(attempt("pat_2"), PriorAttempt::Unterminated, false),
        granted(),
    ) {
        Err(ResumeRefused::AmbiguousPriorAttempt { attempt }) => assert_eq!(attempt, "pat_2"),
        other => panic!("an unterminated attempt is ambiguous, got: {other:?}"),
    }

    // The authority was granted for a DIFFERENT loss than the one reconciled.
    match resume(
        ProcessLossReconciliation::new(
            attempt("pat_9"),
            PriorAttempt::FailedBeforeVisibleOutput,
            false,
        ),
        granted(),
    ) {
        Err(ResumeRefused::AttemptSwap {
            authorized,
            reconciled,
        }) => {
            assert_eq!(authorized, "pat_2");
            assert_eq!(reconciled, "pat_9");
        }
        other => panic!("an authority for another loss is a swap, got: {other:?}"),
    }

    // "Continue as the attempt that died" is not a continuation; a retry is a
    // NEW physical attempt.
    match resume(
        ProcessLossReconciliation::new(
            attempt("pat_2"),
            PriorAttempt::FailedBeforeVisibleOutput,
            false,
        ),
        ContinuationAuthority::grant(attempt("pat_2"), attempt("pat_2")),
    ) {
        Err(ResumeRefused::ContinuationIsTheLostAttempt { attempt }) => {
            assert_eq!(attempt, "pat_2")
        }
        other => panic!("a retry is a new attempt, got: {other:?}"),
    }
}

/// A resumed owner admits exactly the attempt its authority named.
///
/// The reconciliation and the authority answer "may this Step continue"; they
/// do not answer "under which attempt", and a driver that opened a different
/// one would grow the Step's message under an identity nobody authorized. The
/// swap is refused at the attempt-start seam, where it is still preventable.
#[test]
fn a_resumed_owner_refuses_an_attempt_its_authority_did_not_name() {
    let scratch = Scratch::new("resume-swap");
    let store = store();
    let clock = FrozenClock;
    let ids = CountingIds::default();
    let message = match "msg_df0a".parse::<MessageId>() {
        Ok(message) => message,
        Err(error) => panic!("msg_df0a is a valid message identity: {error}"),
    };
    let resume = match ResumedMessage::new(
        message,
        ContentOrdinal::new(1),
        ProcessLossReconciliation::new(
            attempt("pat_2"),
            PriorAttempt::FailedBeforeVisibleOutput,
            false,
        ),
        ContinuationAuthority::grant(attempt("pat_2"), attempt("pat_3")),
    ) {
        Ok(resume) => resume,
        Err(error) => panic!("a reconciled loss admits a continuation: {error}"),
    };
    assert_eq!(resume.continuation_attempt().as_str(), "pat_3");
    let mut owner = match DurableAttemptOwner::elect(
        Arc::clone(&store),
        scratch.artifacts(),
        &clock,
        &ids,
        RecordingSink::default(),
        session(),
        step(1),
        Some(resume),
    ) {
        Ok(owner) => owner,
        Err(error) => panic!("the resumed owner must elect: {error}"),
    };
    match owner.commit(DurableFact::AttemptStarted {
        attempt: attempt("pat_4"),
    }) {
        Err(error) => assert!(
            error.reason.contains("authorized for ONE attempt"),
            "got: {}",
            error.reason
        ),
        Ok(()) => panic!("an unauthorized attempt must not continue the message"),
    }
    // And the authorized one still starts, so the guard refuses the SWAP
    // rather than refusing every attempt.
    match owner.commit(DurableFact::AttemptStarted {
        attempt: attempt("pat_3"),
    }) {
        Ok(()) => {}
        Err(error) => panic!("the authorized attempt must start: {}", error.reason),
    }
}

//! The PUBLIC terminal pair a settled Step projects into assistant
//! history, and its serialized fragments.
//!
//! Split from the dogfood module by the module-size ratchet (the
//! 1,000-line ceiling of grapt-types no_god_files): the projection
//! decision, the one place both fragments come from, and the unit
//! witnesses that stand in for an application.

use super::history;
use grapt_session::StepTerminal;
use grapt_types::ProviderCompletionDisposition;

/// What a process-lost Step's message says (CLAIM-2).
pub(crate) const PROCESS_LOSS_MESSAGE: &str =
    "The server stopped before this answer finished; the text above is incomplete.";

/// The PUBLIC pair a settled Step projects into assistant history: the
/// provider finish reason, and the structured error.
///
/// Split out from the serializer because it is the whole compatibility
/// decision, and a decision that only exists inside a format string cannot be
/// witnessed without standing up an application.
pub(crate) fn public_terminal(
    terminal: &Option<StepTerminal>,
) -> (Option<&'static str>, Option<(&'static str, String)>) {
    match terminal {
        Some(StepTerminal::Ended { completion }) => {
            let finish = match completion {
                ProviderCompletionDisposition::AssertedComplete => "stop",
                ProviderCompletionDisposition::AssertedContinue => "tool-calls",
                ProviderCompletionDisposition::Truncated => "length",
                ProviderCompletionDisposition::Unstated => "unknown",
            };
            (Some(finish), None)
        }
        // An interrupted Step is PUBLICLY an abort.
        //
        // `type` is the frozen oracle's "aborted"
        // (the oracle's `to-session-error.ts`). `message` is its
        // constant (runner/llm.ts:107) and is a CONTRACT, not prose: the
        // retained route does exact string equality on it
        // (packages/tui/src/routes/session/index.tsx:1518), so a reword
        // renders an error box where the screen should read "· interrupted".
        //
        // `finish` is ABSENT. It carries the PROVIDER's finish reason, and a
        // provider that was stopped never gave one.
        //
        // Both interruption sources take the same public value, matching the
        // oracle's posture of keeping the finer distinction internal. The
        // durable fact still carries `by` typed, which is where it is
        // authoritative.
        Some(StepTerminal::Failed {
            reason: grapt_types::StepTerminalReason::Interrupted { by: _ },
        }) => (None, Some(("aborted", "Step interrupted".to_owned()))),
        // A future build's reason: the type stays the named debt, but the
        // message is the SANITIZED stored tag, not a Debug render of a struct
        // this build's vocabulary does not have.
        Some(StepTerminal::Failed {
            reason: grapt_types::StepTerminalReason::Unknown { tag },
        }) => (
            Some("error"),
            Some((
                "StepFailed",
                format!("unknown step failure: {}", history::display_safe(tag, 64)),
            )),
        ),
        Some(StepTerminal::Failed {
            reason: grapt_types::StepTerminalReason::ContextFailure {
                failure: grapt_types::ContextFailure::ContinuityUnavailable,
            },
        }) => (
            Some("error"),
            Some((
                "StepFailed",
                "StateEngine continuity is unavailable. See the Session's recovery message before retrying.".to_owned(),
            )),
        ),
        // CLAIM-2: the server process died mid-Step. Recovery kept whatever
        // text had streamed as the message's content, so the message must
        // read as unfinished: `finish` stays "error", never a provider finish,
        // and the message says the text is incomplete instead of a Debug
        // render. Not the user-interrupt "aborted" contract above, whose
        // message means the USER stopped it.
        Some(StepTerminal::Failed {
            reason: grapt_types::StepTerminalReason::ProcessLoss { .. },
        }) => (
            Some("error"),
            Some(("StepFailed", PROCESS_LOSS_MESSAGE.to_owned())),
        ),
        // DEBT: every other failure publishes the literal type "StepFailed",
        // which is not in the oracle's public vocabulary, and a Debug-rendered
        // reason. This pre-existing presentation debt is outside this correction.
        Some(StepTerminal::Failed { reason }) => {
            (Some("error"), Some(("StepFailed", format!("{reason:?}"))))
        }
        None => (Some("unknown"), None),
    }
}

/// Render the terminal pair into its two JSON fragments.
///
/// Both fragments come from ONE place, so a coordinate cannot be reintroduced
/// or dropped anywhere else in the serializer, and the emitted SHAPE — not
/// only the decision behind it — is witnessable without standing up an
/// application.
pub(crate) fn terminal_fragments(
    finish: Option<&'static str>,
    error: Option<(&'static str, String)>,
) -> Result<(String, String), String> {
    // Absent, not empty: an omitted `finish` and `"finish":""` are different
    // claims, and only one of them is true of an interrupted Step.
    let finish = match finish {
        Some(finish) => format!(",\"finish\":\"{finish}\""),
        None => String::new(),
    };
    let error = match error {
        Some((kind, message)) => {
            let message = serde_json::to_string(&message).map_err(|error| error.to_string())?;
            format!(",\"error\":{{\"type\":\"{kind}\",\"message\":{message}}}")
        }
        None => String::new(),
    };
    Ok((finish, error))
}

#[cfg(test)]
mod public_terminal_tests {
    use super::*;
    use grapt_types::{InterruptionSource, StepTerminalReason};

    #[test]
    fn unavailable_continuity_points_to_recovery_instead_of_debug_vocabulary() {
        let terminal = Some(StepTerminal::Failed {
            reason: StepTerminalReason::ContextFailure {
                failure: grapt_types::ContextFailure::ContinuityUnavailable,
            },
        });
        let (finish, error) = public_terminal(&terminal);
        assert_eq!(finish, Some("error"));
        let (kind, message) = error.unwrap_or_else(|| panic!("missing failure presentation"));
        assert_eq!(kind, "StepFailed");
        assert!(message.contains("recovery message"));
        assert!(!message.contains("ContinuityUnavailable"));
    }

    /// CLAIM-2: a process-lost Step reads as unfinished, in words, and not as
    /// a user interrupt.
    #[test]
    fn a_process_lost_step_says_its_text_is_incomplete_and_is_not_an_abort() {
        for effect in [
            grapt_types::ExternalEffectState::NoneInitiated,
            grapt_types::ExternalEffectState::Ambiguous {
                evidence: grapt_types::AmbiguityEvidence::new("sent with no settlement")
                    .unwrap_or_else(|| panic!("non-empty evidence")),
            },
        ] {
            let terminal = Some(StepTerminal::Failed {
                reason: StepTerminalReason::ProcessLoss { effect },
            });
            let (finish, error) = public_terminal(&terminal);
            assert_eq!(finish, Some("error"), "never a provider finish");
            let (kind, message) = error.unwrap_or_else(|| panic!("missing failure presentation"));
            assert_eq!(kind, "StepFailed");
            assert_eq!(message, PROCESS_LOSS_MESSAGE);
            assert!(!message.contains("ProcessLoss"), "no Debug render");
            assert_ne!(message, "Step interrupted");
        }
    }

    /// The exact public shape of an interrupted Step, in all three
    /// coordinates. Each assertion pins a value a real consumer reads, and each
    /// was wrong in a different way before this repair.
    #[test]
    fn an_interrupted_step_is_aborted_with_the_oracles_message_and_no_finish() {
        for by in [
            InterruptionSource::User,
            InterruptionSource::ManagedShutdown,
        ] {
            let terminal = Some(StepTerminal::Failed {
                reason: StepTerminalReason::Interrupted { by },
            });
            let (finish, error) = public_terminal(&terminal);

            // Was "StepFailed", which is not a value in the oracle's public
            // error vocabulary at all. A client reading `type` called this a
            // failure.
            assert_eq!(
                error.as_ref().map(|(kind, _)| *kind),
                Some("aborted"),
                "the oracle's value (to-session-error.ts), not a minted one"
            );

            // Was already right, and must not drift: the retained route does
            // EXACT string equality on it
            // (packages/tui/src/routes/session/index.tsx:1518), and a near
            // miss renders an error box instead of "· interrupted".
            assert_eq!(
                error.as_ref().map(|(_, message)| message.as_str()),
                Some("Step interrupted"),
                "byte-identical to runner/llm.ts:107 STEP_INTERRUPTED"
            );

            // Was "error". `finish` carries the PROVIDER's finish reason, and
            // a provider that was stopped never gave one.
            assert_eq!(
                finish, None,
                "an interrupted Step has no provider finish to report"
            );
        }
    }

    /// The SERIALIZED shape, not only the decision behind it.
    ///
    /// A unit witness over the tuple cannot see a serializer that reintroduces
    /// `finish`, drops `error`, or renders either differently. Both fragments
    /// now come from one place, and this asserts the bytes that place emits.
    #[test]
    fn the_serialized_fragments_omit_finish_and_carry_aborted_for_an_interrupt() {
        let terminal = Some(StepTerminal::Failed {
            reason: StepTerminalReason::Interrupted {
                by: InterruptionSource::User,
            },
        });
        let (finish, error) = public_terminal(&terminal);
        let (finish, error) = match terminal_fragments(finish, error) {
            Ok(fragments) => fragments,
            Err(reason) => panic!("the terminal fragments must render: {reason}"),
        };
        assert_eq!(
            finish, "",
            "the finish coordinate is OMITTED; an empty string value would be a              different and false claim"
        );
        assert_eq!(
            error, r#","error":{"type":"aborted","message":"Step interrupted"}"#,
            "the exact bytes a consumer reads"
        );
    }

    #[test]
    fn the_serialized_fragments_keep_finish_for_a_completed_step() {
        let terminal = Some(StepTerminal::Ended {
            completion: ProviderCompletionDisposition::AssertedComplete,
        });
        let (finish, error) = public_terminal(&terminal);
        let (finish, error) = match terminal_fragments(finish, error) {
            Ok(fragments) => fragments,
            Err(reason) => panic!("the terminal fragments must render: {reason}"),
        };
        assert_eq!(finish, r#","finish":"stop""#);
        assert_eq!(error, "", "a completed Step carries no error fragment");
    }

    /// COUNTEREXAMPLE ONE: an ordinary failure must NOT become an abort.
    /// A repair that made everything "aborted" would pass the test above and
    /// destroy the distinction it exists to protect.
    #[test]
    fn an_ordinary_failure_is_not_an_abort_and_still_reports_a_finish() {
        let terminal = Some(StepTerminal::Failed {
            reason: StepTerminalReason::DurabilityFailure,
        });
        let (finish, error) = public_terminal(&terminal);
        assert_ne!(error.as_ref().map(|(kind, _)| *kind), Some("aborted"));
        assert_ne!(
            error.as_ref().map(|(_, message)| message.as_str()),
            Some("Step interrupted"),
            "no other terminal may wear the string the retained route keys on"
        );
        assert_eq!(
            finish,
            Some("error"),
            "unchanged: the general error mapping is named debt, not repaired here"
        );
    }

    /// COUNTEREXAMPLE TWO: a clean end is untouched in both coordinates.
    #[test]
    fn a_completed_step_still_reports_its_provider_finish_and_no_error() {
        let terminal = Some(StepTerminal::Ended {
            completion: ProviderCompletionDisposition::AssertedComplete,
        });
        assert_eq!(public_terminal(&terminal), (Some("stop"), None));

        let truncated = Some(StepTerminal::Ended {
            completion: ProviderCompletionDisposition::Truncated,
        });
        assert_eq!(public_terminal(&truncated), (Some("length"), None));
    }

    /// PRESERVED LEGACY BEHAVIOUR, PINNED AS DEBT — not a claim that absence
    /// is preserved.
    ///
    /// An unsettled Step publishes finish "unknown", which is a SETTLED-looking
    /// value for a Step that has not settled. Asserting it does not establish
    /// that absence survives the projection, and a consumer that maps anything
    /// it does not recognise onto success will read this as a completed Step.
    ///
    /// It is pinned so the value cannot drift unnoticed, and named as debt so
    /// the pin is not mistaken for a guarantee. Whether an unsettled Step
    /// should publish an absent finish is an active-message reconciliation
    /// question for the integrated journey, not something this unit settles.
    #[test]
    fn an_unsettled_step_publishes_unknown_which_is_pinned_debt_not_absence() {
        assert_eq!(public_terminal(&None), (Some("unknown"), None));
    }

    /// Tolerant readers: a future build's reason publishes its sanitized
    /// stored tag — a named failure with readable text — never a Debug render
    /// of a struct this build's vocabulary does not have, and never terminal
    /// control bytes.
    #[test]
    fn a_future_reason_publishes_its_sanitized_tag_not_a_debug_render() {
        let tag = "aborted\u{1b}[31m";
        let terminal = Some(StepTerminal::Failed {
            reason: StepTerminalReason::Unknown {
                tag: tag.to_owned(),
            },
        });
        let (finish, error) = public_terminal(&terminal);
        assert_eq!(finish, Some("error"));
        let Some((kind, message)) = error else {
            panic!("the unknown failure must publish an error")
        };
        assert_eq!(kind, "StepFailed");
        assert_eq!(
            message, "unknown step failure: aborted\\u{1b}[31m",
            "the tag is sanitized, not Debug-rendered or re-injected"
        );
    }
}

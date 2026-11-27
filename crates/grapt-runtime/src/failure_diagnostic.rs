//! Why a Step failed, where someone can actually read it.
//!
//! THE PROBLEM THIS SOLVES. `StepFailure` carries both a reason and a DETAIL,
//! and the durable `StepFailedFact` carries only the reason. So a Step that
//! failed `invalid-provider-output` records its class and nothing about which
//! of several very different faults produced it, and the message explaining it
//! is dropped at the boundary that builds the fact.
//!
//! Measured consequence: reading a failed session's durable record afterwards
//! yields the class, plus one generic "The execution stopped before
//! completing". The reason exists in process and is discarded.
//!
//! This does NOT change the durable event or any DTO. It writes one bounded
//! line through bounded diagnostics where the detail is already in hand, so the
//! fault is attributable without a raw provider capture.
//!
//! BOUNDED AND SANITISED. The detail is product-generated text, but it can
//! quote a provider, so it is truncated and stripped of control characters.
//! No credential, prompt or tool argument is read here -- only the failure's
//! own message.

/// Largest detail this will print. Long enough for the messages the runtime
/// actually constructs; short enough that a hostile or enormous provider
/// string cannot fill a log.
const MAX_DETAIL: usize = 512;

/// Emit one line naming the session, step, failure class and detail.
pub(crate) fn step_failed(
    session: &grapt_types::SessionId,
    step: grapt_types::StepNumber,
    failure: &crate::owner::StepFailure,
) {
    let session = session.as_str();
    let step = step.value();
    let reason = failure.reason();
    let detail = failure.detail().as_str();
    // Debug rather than a hand-written label: the reason carries structured
    // fields a label would drop, and inventing a second naming of an existing
    // vocabulary is how the two drift apart.
    let class = format!("{reason:?}");
    let mut sanitised: String = detail
        .chars()
        .map(|character| {
            if character.is_control() {
                ' '
            } else {
                character
            }
        })
        .take(MAX_DETAIL)
        .collect();
    if detail.chars().count() > MAX_DETAIL {
        sanitised.push_str(" …");
    }
    let class: String = class
        .chars()
        .filter(|c| !c.is_control())
        .take(MAX_DETAIL)
        .collect();
    grapt_diagnostics::report(format!(
        "step-failed session={session} step={step} reason={class} detail={sanitised}"
    ));
}

// Probe (R2b-1): a bare string cannot inhabit the durable Step-failure reason.
//
// `session.step.failed` v1 carries `error: {type, message}`, and its one frozen
// D2 row is `{"type": "aborted", ...}`. That string is exactly the carrier R2a
// removed from the runtime, where it had been deciding live control flow. v2
// exists so the durable fact cannot re-admit it.
//
// A runtime assertion that the codec rejects a string is weaker than this: it
// would only prove that one decode path refuses one shape, while a caller
// holding the constructor could still build the fact from a literal. Here the
// value is unnameable.
//
// One error phase: the step number is a REAL, valid `StepNumber`, so the only
// recorded fault is the reason argument. A diverging `todo!()` would also have
// compiled here, but it coerces to any type and emits an unreachable-expression
// warning that clutters the recorded stderr — and in the SUBJECT position it
// would coerce to `StepTerminalReason` and prove nothing at all.

use grapt_event::session_step_failed::StepFailedFact;
use grapt_types::StepNumber;

fn main() {
    let step = match StepNumber::new(1) {
        Some(step) => step,
        None => return,
    };
    let _ = StepFailedFact::new(step, "aborted");
}

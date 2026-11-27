use super::*;

fn allowance(steps: u32) -> StepAllowance {
    match StepAllowance::new(steps) {
        Some(allowance) => allowance,
        None => panic!("allowance must be positive"),
    }
}

#[test]
fn a_zero_step_allowance_is_unrepresentable() {
    assert!(StepAllowance::new(0).is_none());
    assert_eq!(
        StepAllowance::new(1).map(|allowance| allowance.value()),
        Some(1)
    );
}

#[test]
fn a_history_cursor_orders_by_durable_position() {
    assert!(HistoryCursor::new(3) < HistoryCursor::new(4));
    assert_eq!(HistoryCursor::new(7).value(), 7);
}

#[test]
fn a_new_chain_preserves_its_session_step_offset() {
    let step = match StepNumber::new(7) {
        Some(step) => step,
        None => panic!("the non-zero Step must be valid"),
    };
    assert_eq!(initial_step_value(step), 7);
}

#[test]
fn an_allowance_is_spent_per_step_and_refused_when_exhausted() {
    let mut ledger = AllowanceLedger::new(allowance(2));
    assert_eq!(ledger.take(), Ok(()));
    assert_eq!(ledger.take(), Ok(()));
    assert_eq!(ledger.take(), Err(AllowanceExhausted));
}

#[test]
fn a_batch_grants_the_allowance_once_and_a_step_opens_the_next_batch() {
    // Observed on `grants`, NOT on remaining capacity. Capacity cannot
    // distinguish a per-steer reset from a per-batch one: both leave a
    // just-opened batch with a full allowance, because zeroing zero and
    // doing nothing produce the same number.
    let mut ledger = AllowanceLedger::new(allowance(2));
    assert_eq!(ledger.grants, 1, "opening the chain grants the first batch");

    // Two more steers join that batch before any Step runs.
    ledger.admit_input();
    ledger.admit_input();
    assert_eq!(
        ledger.grants, 1,
        "a batch of steers grants once, not once per steer"
    );

    // The Step closes the batch it answered.
    assert_eq!(ledger.take(), Ok(()));
    assert_eq!(ledger.spent, 1);

    // So the next steer is a new user turn, and grants.
    ledger.admit_input();
    assert_eq!(ledger.grants, 2);
    assert_eq!(
        ledger.spent, 0,
        "and the grant refills what the last turn spent"
    );
    ledger.admit_input();
    assert_eq!(
        ledger.grants, 2,
        "the rest of THIS batch still grants nothing"
    );

    assert_eq!(ledger.take(), Ok(()));
    assert_eq!(ledger.take(), Ok(()));
    assert_eq!(ledger.take(), Err(AllowanceExhausted));
}

#[test]
fn a_looping_model_exhausts_the_allowance_while_a_steering_user_refills_it() {
    // The distinction the whole ledger exists to draw, end to end.
    let mut ledger = AllowanceLedger::new(allowance(2));

    // The model keeps calling tools: Steps with no user input between them.
    assert_eq!(ledger.take(), Ok(()));
    assert_eq!(ledger.take(), Ok(()));
    assert_eq!(ledger.take(), Err(AllowanceExhausted), "the loop stops");
    assert_eq!(
        ledger.grants, 1,
        "and nothing along the way granted it more"
    );

    // A user asking for something else is a new turn, and the chain runs on.
    ledger.admit_input();
    assert_eq!(ledger.grants, 2);
    assert_eq!(ledger.take(), Ok(()));
}

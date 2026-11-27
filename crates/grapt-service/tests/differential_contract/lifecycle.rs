use super::*;

// ---------------------------------------------------------------------------
// Restart, resume and long-session identity laws
// ---------------------------------------------------------------------------

#[test]
fn an_engine_that_kept_its_instance_identity_did_not_restart() {
    let before = rust_owner();
    // Same slot, same instance, same environment — only the PID moved. A new
    // PID is exactly the evidence a naive restart check would accept.
    let after = owner(
        "slot_desktop",
        "inst_rust_a",
        "env_rust",
        9999,
        "http://127.0.0.1:4101",
    );
    assert_ne!(
        before.pid().value(),
        after.pid().value(),
        "the case is only meaningful if the PID actually changed"
    );
    expect_refusal(
        RestartBoundary::observe(before, after),
        &DifferentialRefusal::RestartKeptTheInstanceIdentity,
        "a restart claim with an unchanged instance identity",
    );
}

#[test]
fn an_engine_that_came_back_with_another_environment_receipt_is_a_replacement_not_a_restart() {
    // Owner identity is {slot, instance, environment}. Checking only the slot
    // would let `after` name a different owner on the same slot — a
    // replacement, which has its own landed vocabulary this crate never
    // produces. Pinning the environment here is what makes `after` carried by
    // `before`, and therefore transitively bound to an admitted engine once
    // `before` is.
    let before = rust_owner();
    let after = owner(
        "slot_desktop",
        "inst_rust_b",
        "env_rust_but_rebuilt",
        4101,
        "http://127.0.0.1:4101",
    );
    // Same slot and a genuinely new instance, so the other two restart laws are
    // satisfied and only the environment separates this case.
    assert_eq!(before.slot(), after.slot());
    assert_ne!(before.instance(), after.instance());
    expect_refusal(
        RestartBoundary::observe(before, after),
        &DifferentialRefusal::RestartChangedTheEnvironment,
        "a restart claim across two environment receipts",
    );
}

#[test]
fn an_engine_that_came_back_on_another_slot_is_a_different_service_not_a_restart() {
    let before = rust_owner();
    let after = owner(
        "slot_somewhere_else",
        "inst_rust_b",
        "env_rust",
        4103,
        "http://127.0.0.1:4103",
    );
    expect_refusal(
        RestartBoundary::observe(before, after),
        &DifferentialRefusal::RestartChangedTheSlot,
        "a restart claim across two slots",
    );
}

#[test]
fn a_new_instance_on_the_same_slot_is_admitted_as_a_restart() {
    let before = rust_owner();
    let after = owner(
        "slot_desktop",
        "inst_rust_b",
        "env_rust",
        4101,
        "http://127.0.0.1:4101",
    );
    // The PID is deliberately IDENTICAL to the pre-restart one. A restarted
    // process that inherited its predecessor's PID is still a new instance, and
    // this is the positive half of the reused-PID law.
    assert_eq!(
        before.pid().value(),
        after.pid().value(),
        "the case is only meaningful if the PID was reused"
    );
    match RestartBoundary::observe(before, after) {
        Ok(boundary) => assert_ne!(
            boundary.before().instance(),
            boundary.after().instance(),
            "an admitted restart must have crossed an instance identity"
        ),
        Err(refusal) => panic!("a new instance on the same slot must admit: {refusal}"),
    }
}

#[test]
fn resume_that_reuses_the_recorded_receipt_is_refused() {
    let recorded = receipt(EngineSelection::Rust, rust_owner(), 1_000);
    expect_refusal(
        ResumeReadmission::readmit(recorded.clone(), recorded),
        &DifferentialRefusal::ResumeReusedTheRecordedReceipt,
        "a resume that assumed the recorded attachment survived",
    );
}

#[test]
fn resume_that_re_admits_from_a_fresh_probe_is_accepted_even_for_the_same_owner() {
    // Same engine, same owner — only the admitting instant differs, because a
    // genuine re-admission takes a fresh instant from the injected clock. The
    // law is about re-admitting, not about the owner having changed.
    let before = receipt(EngineSelection::Rust, rust_owner(), 1_000);
    let after = receipt(EngineSelection::Rust, rust_owner(), 5_000);
    match ResumeReadmission::readmit(before, after) {
        Ok(readmission) => assert_ne!(
            readmission.before_sleep().selected_at().as_millis(),
            readmission.after_resume().selected_at().as_millis(),
            "a re-admission must carry a fresh instant"
        ),
        Err(refusal) => panic!("a fresh re-admission must be accepted: {refusal}"),
    }
}

#[test]
fn a_long_session_must_be_bounded_in_both_counts_and_bytes() {
    expect_refusal(
        LongSessionBound::declare(0, 4_096),
        &DifferentialRefusal::LongSessionReadsUnbounded,
        "a long session with an unbounded read count",
    );
    expect_refusal(
        LongSessionBound::declare(16, 0),
        &DifferentialRefusal::LongSessionBytesUnbounded,
        "a long session with an unbounded byte budget",
    );
    match LongSessionBound::declare(16, 4_096) {
        Ok(bound) => {
            assert_eq!(bound.max_reads().get(), 16);
            assert_eq!(bound.max_bytes().get(), 4_096);
        }
        Err(refusal) => panic!("a bound stated in counts and bytes must declare: {refusal}"),
    }
}

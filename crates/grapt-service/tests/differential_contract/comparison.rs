use super::*;

// ---------------------------------------------------------------------------
// Plan-level relations
// ---------------------------------------------------------------------------

#[test]
fn a_subset_declared_for_one_matrix_cannot_be_planned_as_another() {
    // Sleep/resume's minimum is a SUPERSET of restart's, so this subset would
    // satisfy a restart plan's completeness check on its contents alone. Only
    // the declared matrix separates them, which is what makes this law
    // load-bearing rather than redundant with subset completeness.
    let resume_subset = subset(DifferentialMatrix::SleepResume);
    expect_refusal(
        DifferentialPlan::admit(
            DifferentialMatrix::Restart,
            resume_subset,
            admitted_pair(),
            recorded_bounds(),
            MatrixEvidence::Restart(restart_boundary()),
            SelectorState::Retained,
        ),
        &DifferentialRefusal::SubsetDeclaredForAnotherMatrix {
            declared: DifferentialMatrix::SleepResume,
            planned: DifferentialMatrix::Restart,
        },
        "a subset borrowed from another matrix",
    );
}

fn restart_boundary() -> RestartBoundary {
    let after = owner(
        "slot_desktop",
        "inst_rust_b",
        "env_rust",
        4101,
        "http://127.0.0.1:4101",
    );
    match RestartBoundary::observe(rust_owner(), after) {
        Ok(boundary) => boundary,
        Err(refusal) => panic!("a new instance on the same slot must admit: {refusal}"),
    }
}

#[test]
fn evidence_from_another_matrix_cannot_support_a_plan() {
    expect_refusal(
        DifferentialPlan::admit(
            DifferentialMatrix::Differential,
            subset(DifferentialMatrix::Differential),
            admitted_pair(),
            recorded_bounds(),
            MatrixEvidence::Restart(restart_boundary()),
            SelectorState::Retained,
        ),
        &DifferentialRefusal::MatrixEvidenceMismatch {
            planned: DifferentialMatrix::Differential,
            evidence: DifferentialMatrix::Restart,
        },
        "a differential plan carrying restart evidence",
    );
}

/// An owner for a service neither admitted engine names.
fn stranger_owner(instance: &str, pid: u32, endpoint: &str) -> OwnerReceipt {
    owner(
        "slot_unrelated_service",
        instance,
        "env_stranger",
        pid,
        endpoint,
    )
}

#[test]
fn restart_evidence_for_a_service_the_plan_did_not_admit_is_refused() {
    // The R1 review's counterexample, now a law. The boundary below is a
    // perfectly well-formed restart — same slot, same environment, new
    // instance — of a service that is neither admitted engine. A component
    // cannot catch this: `RestartBoundary::observe` is never given the engine
    // pair, so only the plan can relate them.
    let boundary = match RestartBoundary::observe(
        stranger_owner("inst_stranger_a", 7001, "http://127.0.0.1:7001"),
        stranger_owner("inst_stranger_b", 7002, "http://127.0.0.1:7002"),
    ) {
        Ok(boundary) => boundary,
        Err(refusal) => panic!("the stranger's own restart must be well-formed: {refusal}"),
    };
    expect_refusal(
        DifferentialPlan::admit(
            DifferentialMatrix::Restart,
            subset(DifferentialMatrix::Restart),
            admitted_pair(),
            recorded_bounds(),
            MatrixEvidence::Restart(boundary),
            SelectorState::Retained,
        ),
        &DifferentialRefusal::MatrixEvidenceNamesAnotherEngine,
        "a restart of a third, unrelated service",
    );
}

#[test]
fn restart_evidence_for_an_admitted_engine_is_accepted() {
    // The positive control: identical in every respect except that the boundary
    // now starts from an owner the plan actually admitted. Without this, the
    // refusal above could be coming from something incidental to the case.
    match DifferentialPlan::admit(
        DifferentialMatrix::Restart,
        subset(DifferentialMatrix::Restart),
        admitted_pair(),
        recorded_bounds(),
        MatrixEvidence::Restart(restart_boundary()),
        SelectorState::Retained,
    ) {
        Ok(plan) => assert_eq!(plan.matrix(), DifferentialMatrix::Restart),
        Err(refusal) => panic!("a restart of an admitted engine must admit: {refusal}"),
    }
}

#[test]
fn resume_evidence_for_a_service_the_plan_did_not_admit_is_refused() {
    let before = receipt(
        EngineSelection::Rust,
        stranger_owner("inst_stranger_a", 7001, "http://127.0.0.1:7001"),
        3_000,
    );
    let after = receipt(
        EngineSelection::Rust,
        stranger_owner("inst_stranger_a", 7001, "http://127.0.0.1:7001"),
        9_000,
    );
    let readmission = match ResumeReadmission::readmit(before, after) {
        Ok(readmission) => readmission,
        Err(refusal) => panic!("the stranger's own re-admission must be well-formed: {refusal}"),
    };
    expect_refusal(
        DifferentialPlan::admit(
            DifferentialMatrix::SleepResume,
            subset(DifferentialMatrix::SleepResume),
            admitted_pair(),
            recorded_bounds(),
            MatrixEvidence::Resume(readmission),
            SelectorState::Retained,
        ),
        &DifferentialRefusal::MatrixEvidenceNamesAnotherEngine,
        "a resume of a third, unrelated service",
    );
}

#[test]
fn resume_evidence_split_across_the_two_admitted_engines_is_refused() {
    // Both sides are admitted owners, so an "each side is admitted" test would
    // pass this. It still describes no single engine's sleep boundary, which is
    // why the law names ONE owner rather than membership twice.
    let before = receipt(EngineSelection::Rust, rust_owner(), 3_000);
    let after = receipt(EngineSelection::Rust, retained_owner(), 9_000);
    let readmission = match ResumeReadmission::readmit(before, after) {
        Ok(readmission) => readmission,
        Err(refusal) => panic!("two distinct receipts must re-admit: {refusal}"),
    };
    expect_refusal(
        DifferentialPlan::admit(
            DifferentialMatrix::SleepResume,
            subset(DifferentialMatrix::SleepResume),
            admitted_pair(),
            recorded_bounds(),
            MatrixEvidence::Resume(readmission),
            SelectorState::Retained,
        ),
        &DifferentialRefusal::MatrixEvidenceNamesAnotherEngine,
        "a resume whose two halves name two different admitted engines",
    );
}

/// Build a sleep/resume plan from two `(selection, owner, instant)` triples.
fn resume_plan(
    before: (EngineSelection, OwnerReceipt, i64),
    after: (EngineSelection, OwnerReceipt, i64),
) -> Result<DifferentialPlan, DifferentialRefusal> {
    let readmission = match ResumeReadmission::readmit(
        receipt(before.0, before.1, before.2),
        receipt(after.0, after.1, after.2),
    ) {
        Ok(readmission) => readmission,
        Err(refusal) => panic!("two distinct receipts must re-admit: {refusal}"),
    };
    DifferentialPlan::admit(
        DifferentialMatrix::SleepResume,
        subset(DifferentialMatrix::SleepResume),
        admitted_pair(),
        recorded_bounds(),
        MatrixEvidence::Resume(readmission),
        SelectorState::Retained,
    )
}

#[test]
fn resume_evidence_agreeing_on_an_engine_the_owner_was_not_admitted_as_is_refused() {
    // The R2 closure counterexample, now a law. `admitted_pair()` admits
    // Rust <- rust_owner and Retained <- retained_owner. Both halves below name
    // rust_owner — ONE admitted owner, so the ownership test passes — and both
    // select Retained, so they agree with each other. They agree on the wrong
    // engine, and the durable row would carry `selection = "retained"` beside
    // the Rust engine's identity coordinates.
    //
    // This is why the rule is stated against the admitted RECEIPT rather than
    // against the owner set plus internal agreement: agreement between the two
    // halves says nothing about the pairing they claim to describe.
    expect_refusal(
        resume_plan(
            (EngineSelection::Retained, rust_owner(), 3_000),
            (EngineSelection::Retained, rust_owner(), 9_000),
        ),
        &DifferentialRefusal::ResumeContradictsTheAdmittedPairing,
        "a resume attributing the Rust engine's owner to the Retained engine",
    );
}

#[test]
fn resume_evidence_that_changes_which_engine_it_names_is_refused() {
    // Same owner on both sides, but the two receipts disagree about which
    // engine that owner is. Reclassified at R2 closure: the defect is that the
    // second half contradicts the admitted pairing for rust_owner, which is a
    // sharper statement than "the halves disagree" — the halves disagreeing is
    // only ever a symptom of at least one of them being wrong about the pairing.
    expect_refusal(
        resume_plan(
            (EngineSelection::Rust, rust_owner(), 3_000),
            (EngineSelection::Retained, rust_owner(), 9_000),
        ),
        &DifferentialRefusal::ResumeContradictsTheAdmittedPairing,
        "a resume that changes which engine it names",
    );
}

#[test]
fn resume_evidence_matching_the_admitted_pairing_is_accepted() {
    // The positive control for both refusals above: same owner, and the
    // selection that owner was actually admitted as. Differs from the R2
    // counterexample in exactly one coordinate — the selection.
    match resume_plan(
        (EngineSelection::Rust, rust_owner(), 3_000),
        (EngineSelection::Rust, rust_owner(), 9_000),
    ) {
        Ok(plan) => assert_eq!(plan.matrix(), DifferentialMatrix::SleepResume),
        Err(refusal) => panic!("a resume matching the admitted pairing must admit: {refusal}"),
    }

    // And the same for the OTHER admitted engine, so the law is not passing by
    // hard-coding the left-hand side of the pair.
    match resume_plan(
        (EngineSelection::Retained, retained_owner(), 3_000),
        (EngineSelection::Retained, retained_owner(), 9_000),
    ) {
        Ok(plan) => assert_eq!(plan.matrix(), DifferentialMatrix::SleepResume),
        Err(refusal) => panic!("the retained engine's own resume must admit too: {refusal}"),
    }
}

#[test]
fn long_session_and_differential_evidence_carry_no_owner_so_nothing_is_related() {
    // Stated as a law rather than left as an absence: these two matrices carry
    // no owner to relate, and a long-session bound is a statement about the
    // driver's own reads, not about an engine. If the relation check ever
    // started refusing them it would be refusing a plan for a reason that does
    // not exist.
    for (matrix, evidence) in [
        (DifferentialMatrix::Differential, MatrixEvidence::None),
        (DifferentialMatrix::LongSession, {
            match LongSessionBound::declare(16, 4_096) {
                Ok(bound) => MatrixEvidence::LongSession(bound),
                Err(refusal) => panic!("a bounded long session must declare: {refusal}"),
            }
        }),
    ] {
        match DifferentialPlan::admit(
            matrix,
            subset(matrix),
            admitted_pair(),
            recorded_bounds(),
            evidence,
            SelectorState::Retained,
        ) {
            Ok(plan) => assert_eq!(plan.matrix(), matrix),
            Err(refusal) => panic!("{}: must admit: {refusal}", matrix.label()),
        }
    }
}

#[test]
fn a_plan_that_leaves_the_selector_attached_is_refused() {
    let attached = SelectorState::attach(receipt(EngineSelection::Rust, rust_owner(), 3_000));
    expect_refusal(
        DifferentialPlan::admit(
            DifferentialMatrix::Differential,
            subset(DifferentialMatrix::Differential),
            admitted_pair(),
            recorded_bounds(),
            MatrixEvidence::None,
            attached,
        ),
        &DifferentialRefusal::LeavesSelectorAttached,
        "a plan that strands the desktop on a borrowed engine",
    );
}

#[test]
fn reverting_an_attached_selector_makes_the_same_plan_admissible() {
    // The positive control for the reversibility law, and the reason it is not
    // vacuous: the ONLY difference from the refused case above is that the
    // terminal selector was reverted. R4D-1 proves `revert` is always
    // available; this proves the contract requires the driver to use it.
    let attached = SelectorState::attach(receipt(EngineSelection::Rust, rust_owner(), 3_000));
    let reverted = attached.revert();
    assert_eq!(reverted, SelectorState::Retained);
    match DifferentialPlan::admit(
        DifferentialMatrix::Differential,
        subset(DifferentialMatrix::Differential),
        admitted_pair(),
        recorded_bounds(),
        MatrixEvidence::None,
        reverted,
    ) {
        Ok(plan) => assert_eq!(plan.terminal_selector(), &SelectorState::Retained),
        Err(refusal) => panic!("a reverted plan must admit: {refusal}"),
    }
}

#[test]
fn every_matrix_admits_a_complete_plan_and_all_four_are_reachable() {
    let cases = [
        (DifferentialMatrix::Differential, MatrixEvidence::None),
        (
            DifferentialMatrix::Restart,
            MatrixEvidence::Restart(restart_boundary()),
        ),
        (DifferentialMatrix::SleepResume, {
            let before = receipt(EngineSelection::Rust, rust_owner(), 1_000);
            let after = receipt(EngineSelection::Rust, rust_owner(), 6_000);
            match ResumeReadmission::readmit(before, after) {
                Ok(readmission) => MatrixEvidence::Resume(readmission),
                Err(refusal) => panic!("a fresh re-admission must be accepted: {refusal}"),
            }
        }),
        (DifferentialMatrix::LongSession, {
            match LongSessionBound::declare(16, 4_096) {
                Ok(bound) => MatrixEvidence::LongSession(bound),
                Err(refusal) => panic!("a bounded long session must declare: {refusal}"),
            }
        }),
    ];

    for (matrix, evidence) in cases {
        match DifferentialPlan::admit(
            matrix,
            subset(matrix),
            admitted_pair(),
            recorded_bounds(),
            evidence,
            SelectorState::Retained,
        ) {
            Ok(plan) => {
                assert_eq!(plan.matrix(), matrix);
                assert_eq!(
                    plan.rust_bounds().bounds().len(),
                    HostBoundName::ALL.len(),
                    "{}: an admitted plan carries every recorded host bound",
                    matrix.label()
                );
            }
            Err(refusal) => panic!("{}: a complete plan must admit: {refusal}", matrix.label()),
        }
    }
    assert_eq!(
        DifferentialMatrix::ALL.len(),
        4,
        "the four matrices R4 owes are exactly the four covered here"
    );
}

// ---------------------------------------------------------------------------
// The no-duplication claim, checked mechanically
// ---------------------------------------------------------------------------

#[test]
fn attachment_laws_reach_this_lanes_projection_in_r4d1s_own_words() {
    // Each of the four attachment-level refusals this lane's admission names is
    // produced by R4D-1's own constructors, converted with `?`-style `From`,
    // and must keep R4D-1's label. If this lane ever minted a second spelling
    // for one of them, the label would change here.
    let health_only = SelectionReceipt::admit(
        EngineSelection::Rust,
        ProbeOutcome::HealthyUnattributed,
        &FixedClock(1_000),
    );
    let health_only: DifferentialRefusal = match health_only {
        Ok(_) => panic!("an unattributed probe must not admit"),
        Err(refusal) => refusal.into(),
    };
    assert_eq!(health_only.label(), "unattributed");
    assert_eq!(
        health_only,
        DifferentialRefusal::Attachment(AttachmentRefusal::Unattributed)
    );

    // Stale selector / reused PID: the same law seen from two sides. A receipt
    // recorded against one owner does not describe a different instance that
    // inherited its PID and endpoint.
    let recorded = receipt(EngineSelection::Rust, rust_owner(), 1_000);
    let impostor = owner(
        "slot_desktop",
        "inst_rust_b",
        "env_rust",
        4101,
        "http://127.0.0.1:4101",
    );
    let stale: DifferentialRefusal = match recorded.still_current(&impostor) {
        Ok(()) => panic!("a different instance reusing a PID must not be the recorded owner"),
        Err(refusal) => refusal.into(),
    };
    assert_eq!(stale.label(), "not-the-recorded-owner");

    // Same owner on both sides: one engine measured twice.
    let left = receipt(EngineSelection::Rust, rust_owner(), 1_000);
    let right = receipt(EngineSelection::Retained, rust_owner(), 2_000);
    let same_owner: DifferentialRefusal = match AdmittedDifferential::admit(left, right) {
        Ok(_) => panic!("one engine measured twice must not admit as a differential"),
        Err(refusal) => refusal.into(),
    };
    assert_eq!(same_owner.label(), "same-owner-on-both-sides");

    // Same selection on both sides: not a comparison at all.
    let left = receipt(EngineSelection::Rust, rust_owner(), 1_000);
    let right = receipt(EngineSelection::Rust, retained_owner(), 2_000);
    let same_selection: DifferentialRefusal = match AdmittedDifferential::admit(left, right) {
        Ok(_) => panic!("two receipts naming one engine must not admit as a differential"),
        Err(refusal) => refusal.into(),
    };
    assert_eq!(same_selection.label(), "same-selection-on-both-sides");
}

/// One representative value of **every** variant this contract can produce.
///
/// The R1 review falsified the previous shape of this list: it was compared
/// against a hand-written literal of labels, so a nineteenth variant with a
/// label and no corpus row left the whole suite green. This list is still
/// written by hand — there is no way to enumerate an enum's variants in Rust —
/// but it is no longer the *authority*. `DifferentialRefusal::label_index` is,
/// and its match is exhaustive over this enum and over the `AttachmentRefusal`
/// it carries, so a new variant is a **compile error** rather than a silent
/// pass. `every_label_in_the_carrier_is_produced_by_a_real_variant` then proves
/// this list still covers the carrier, which is what stops it rotting.
fn one_of_every_refusal() -> Vec<DifferentialRefusal> {
    vec![
        DifferentialRefusal::Attachment(AttachmentRefusal::Unattributed),
        DifferentialRefusal::Attachment(AttachmentRefusal::NotTheRecordedOwner),
        DifferentialRefusal::Attachment(AttachmentRefusal::SameOwnerOnBothSides),
        DifferentialRefusal::Attachment(AttachmentRefusal::SameSelectionOnBothSides),
        DifferentialRefusal::MatrixSubsetIncomplete {
            matrix: DifferentialMatrix::Differential,
            missing: Vec::new(),
        },
        DifferentialRefusal::OperationDeclaredTwice {
            matrix: DifferentialMatrix::Differential,
            operation: Operation::HealthGet,
        },
        DifferentialRefusal::SubsetDeclaredForAnotherMatrix {
            declared: DifferentialMatrix::Restart,
            planned: DifferentialMatrix::Differential,
        },
        DifferentialRefusal::MatrixEvidenceMismatch {
            planned: DifferentialMatrix::Differential,
            evidence: DifferentialMatrix::Restart,
        },
        DifferentialRefusal::MatrixEvidenceNamesAnotherEngine,
        DifferentialRefusal::ResumeContradictsTheAdmittedPairing,
        DifferentialRefusal::BoundNotReportedByHost {
            name: HostBoundName::MaxHeaders,
            provenance: BoundProvenance::AssertedByDriver,
        },
        DifferentialRefusal::BoundSetIncomplete {
            missing: Vec::new(),
        },
        DifferentialRefusal::BoundRecordedTwice {
            name: HostBoundName::MaxHeaders,
        },
        DifferentialRefusal::BoundRenderingEmpty {
            name: HostBoundName::MaxHeaders,
        },
        DifferentialRefusal::RestartKeptTheInstanceIdentity,
        DifferentialRefusal::RestartChangedTheSlot,
        DifferentialRefusal::RestartChangedTheEnvironment,
        DifferentialRefusal::ResumeReusedTheRecordedReceipt,
        DifferentialRefusal::LongSessionReadsUnbounded,
        DifferentialRefusal::LongSessionBytesUnbounded,
        DifferentialRefusal::LeavesSelectorAttached,
    ]
}

#[test]
fn every_label_in_the_carrier_is_produced_by_a_real_variant() {
    // The half that stops `ALL_LABELS` from growing a word no variant produces,
    // and stops the representative list from silently falling behind the enum.
    // Adding a variant is already a compile error in `label_index`; once the
    // author fixes that, this forces the array and this list to grow with it.
    let mut hit = [false; DifferentialRefusal::LABEL_COUNT];
    for refusal in one_of_every_refusal() {
        let index = refusal.label_index();
        assert!(
            index < DifferentialRefusal::LABEL_COUNT,
            "{:?} maps to index {index}, past the end of ALL_LABELS",
            refusal.label()
        );
        assert!(
            !hit[index],
            "index {index} is claimed by two variants; every law needs its own slot"
        );
        hit[index] = true;
    }
    let unclaimed: Vec<&str> = hit
        .iter()
        .enumerate()
        .filter(|(_, claimed)| !**claimed)
        .map(|(index, _)| DifferentialRefusal::ALL_LABELS[index])
        .collect();
    assert!(
        unclaimed.is_empty(),
        "ALL_LABELS carries {unclaimed:?}, which no variant in this test produces: \
         either the array grew a word the enum cannot emit, or a new variant was added \
         without being given a representative here"
    );
}

#[test]
fn the_carrier_and_the_label_agree_for_every_variant() {
    // `label()` keeps its own delegating match — that delegation to R4D-1's
    // `inner.label()` IS the no-duplication proof, and QS18 mutates exactly that
    // arm. So the two matches must be cross-checked, or they can drift apart
    // silently and the corpus would record one while the carrier promised the
    // other.
    for refusal in one_of_every_refusal() {
        assert_eq!(
            DifferentialRefusal::ALL_LABELS[refusal.label_index()],
            refusal.label(),
            "the carrier and label() disagree for {refusal:?}"
        );
    }
}

#[test]
fn no_refusal_label_is_spelled_twice_across_the_whole_vocabulary() {
    // The mechanical form of "this lane duplicates no landed law". If a variant
    // added here ever re-spelled an attachment refusal — or two of this lane's
    // own laws collapsed onto one word — the recorded corpus would stop being
    // able to tell those laws apart, and this fails.
    //
    // Reads the CARRIER, not a literal of its own: the R1 review found that a
    // test comparing a hand list against itself cannot notice the enum growing.
    let labels = DifferentialRefusal::ALL_LABELS;

    let mut seen: Vec<&str> = Vec::new();
    for label in labels {
        assert!(
            !seen.contains(&label),
            "the refusal label {label:?} is spelled twice: two laws sharing one word cannot be told apart in the corpus"
        );
        assert!(!label.is_empty(), "a refusal must carry a label");
        seen.push(label);
    }
    assert_eq!(
        seen.len(),
        DifferentialRefusal::LABEL_COUNT,
        "every slot in the carrier must hold a distinct label"
    );
}

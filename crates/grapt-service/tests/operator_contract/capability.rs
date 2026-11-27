// --------------------------------------------------------------------------
// Custody.
// --------------------------------------------------------------------------

#[test]
fn a_receipt_root_inside_the_repository_is_refused() {
    assert_eq!(
        ReceiptRoot::declare("C:/workspace/repo", "C:/workspace/repo/receipts"),
        Err(OperatorRefusal::ReceiptRootInsideRepository {
            receipt_root: "C:/workspace/repo/receipts".to_owned(),
        }),
        "writing any file into the tree while a battery runs voids the run it was recording"
    );
}

#[test]
fn a_sibling_directory_sharing_the_repository_prefix_is_a_legal_receipt_root() {
    match ReceiptRoot::declare("C:/workspace/repo", "C:/workspace/repo-receipts") {
        Ok(root) => assert_eq!(
            root.receipt_root(),
            "C:/workspace/repo-receipts",
            "a string-prefix test would refuse this legal root and push an operator into moving a directory"
        ),
        Err(refusal) => panic!(
            "a sibling directory is not inside the repository and must be admitted, got {refusal}"
        ),
    }
}

#[test]
fn a_session_body_artifact_may_not_be_retained() {
    assert_eq!(
        DeclaredArtifact::declare("bodies.jsonl", ArtifactClass::SessionBodyContent, 1024),
        Err(OperatorRefusal::ArtifactClassNotRetainable {
            class: ArtifactClass::SessionBodyContent,
        }),
        "session bodies are workspace state and must never reach a run receipt"
    );
}

#[test]
fn a_credential_bearing_registration_may_not_be_retained() {
    assert_eq!(
        DeclaredArtifact::declare(
            "service.json",
            ArtifactClass::CredentialBearingRegistration,
            1024
        ),
        Err(OperatorRefusal::ArtifactClassNotRetainable {
            class: ArtifactClass::CredentialBearingRegistration,
        }),
        "every registration observed on this host carries a password field"
    );
}

#[test]
fn custody_over_the_evidence_ceiling_is_refused() {
    let root = match ReceiptRoot::declare("C:/workspace/repo", "C:/fixtures") {
        Ok(root) => root,
        Err(refusal) => panic!("setup for the over-budget witness failed with {refusal}"),
    };
    let artifacts = vec![declared(
        "oversized.jsonl",
        ArtifactClass::ShapeAndStatusObservation,
        ArtifactCustody::MAX_TOTAL_BYTES + 1,
    )];
    assert_eq!(
        ArtifactCustody::declare(root, artifacts),
        Err(OperatorRefusal::CustodyBudgetExceeded {
            declared: ArtifactCustody::MAX_TOTAL_BYTES + 1,
            ceiling: ArtifactCustody::MAX_TOTAL_BYTES,
        }),
        "evidence is a digest and a sample, and a declaration above the ceiling is a raw stream by another name"
    );
}

/// The accumulator's own law, which the single over-ceiling artifact above
/// never reaches: one artifact can be refused by the ceiling comparison without
/// any addition ever being performed.
///
/// Two declarations whose budgets sum past `u64::MAX` are constructible today —
/// `DeclaredArtifact::declare` accepts any non-zero `u64` — so this is a real
/// input and not a synthetic one.
#[test]
fn two_budgets_that_overflow_the_sum_are_refused_before_the_ceiling_comparison() {
    let root = match ReceiptRoot::declare("C:/workspace/repo", "C:/fixtures") {
        Ok(root) => root,
        Err(refusal) => panic!("setup for the overflow witness failed with {refusal}"),
    };
    let artifacts = vec![
        declared(
            "first.jsonl",
            ArtifactClass::ShapeAndStatusObservation,
            u64::MAX,
        ),
        declared("second.jsonl", ArtifactClass::ShapeAndStatusObservation, 1),
    ];
    assert_eq!(
        ArtifactCustody::declare(root, artifacts),
        Err(OperatorRefusal::CustodyBudgetNotRepresentable {
            name: "second.jsonl".to_owned(),
            preceding: u64::MAX,
        }),
        "a wrapping sum reports a small total here and admits an enormous declaration: the sum must be checked, and the refusal must be the one that says the total is not a number"
    );
}

/// The same overflow, asserted through the refusal's PAYLOAD rather than its
/// variant.
///
/// This is the witness a saturating sum cannot survive. Saturating produces the
/// ordinary over-ceiling refusal carrying `u64::MAX` as a `declared` total that
/// nobody declared; the checked sum names the declaration that could not be
/// added and the total that preceded it. Asserting the payload is what tells
/// those two apart — a witness that only asserted "some refusal happened" would
/// pass under both.
#[test]
fn the_overflow_refusal_names_the_declaration_that_could_not_be_added() {
    let root = match ReceiptRoot::declare("C:/workspace/repo", "C:/fixtures") {
        Ok(root) => root,
        Err(refusal) => panic!("setup for the overflow-payload witness failed with {refusal}"),
    };
    let artifacts = vec![
        declared(
            "half.jsonl",
            ArtifactClass::ShapeAndStatusObservation,
            u64::MAX / 2 + 1,
        ),
        declared(
            "rest.jsonl",
            ArtifactClass::ShapeAndStatusObservation,
            u64::MAX / 2 + 1,
        ),
    ];
    assert_eq!(
        ArtifactCustody::declare(root, artifacts),
        Err(OperatorRefusal::CustodyBudgetNotRepresentable {
            name: "rest.jsonl".to_owned(),
            preceding: u64::MAX / 2 + 1,
        }),
        "the refusal must carry the exact declaration that overflowed and the exact total before it: a saturating sum reports a capped ceiling breach of a figure nobody declared"
    );
}

#[test]
fn custody_that_retains_nothing_is_refused() {
    let root = match ReceiptRoot::declare("C:/workspace/repo", "C:/fixtures") {
        Ok(root) => root,
        Err(refusal) => panic!("setup for the empty-custody witness failed with {refusal}"),
    };
    assert_eq!(
        ArtifactCustody::declare(root, Vec::new()),
        Err(OperatorRefusal::CustodyRetainsNothing),
        "a run that keeps nothing has no evidence, and that state must not be reachable by omission"
    );
}

#[test]
fn two_artifacts_sharing_a_name_are_refused() {
    let root = match ReceiptRoot::declare("C:/workspace/repo", "C:/fixtures") {
        Ok(root) => root,
        Err(refusal) => panic!("setup for the duplicate-artifact witness failed with {refusal}"),
    };
    let artifacts = vec![
        declared("run-receipt.md", ArtifactClass::RunReceipt, 1024),
        declared("run-receipt.md", ArtifactClass::CleanupEnumeration, 1024),
    ];
    assert_eq!(
        ArtifactCustody::declare(root, artifacts),
        Err(OperatorRefusal::ArtifactDeclaredTwice {
            name: "run-receipt.md".to_owned(),
        }),
        "a cleanup that pulls one of two same-named artifacts destroys the other"
    );
}

// --------------------------------------------------------------------------
// Cleanup discipline and timeouts.
// --------------------------------------------------------------------------

#[test]
fn a_kill_by_image_name_is_refused() {
    assert_eq!(
        KillDiscipline::ByImageName("node.exe".to_owned()).admit(),
        Err(OperatorRefusal::ImageNameKillRefused {
            image: "node.exe".to_owned(),
        }),
        "the node and bash processes on this host are other seats, not this run"
    );
}

#[test]
fn a_zero_pid_kill_target_is_refused_separately_from_an_image_name_kill() {
    assert_eq!(
        KillDiscipline::PidScoped(0).admit(),
        Err(OperatorRefusal::KillTargetNotAProcess),
        "a zero pid is unattributable rather than dangerous, which is a different defect"
    );
}

#[test]
fn a_per_matrix_timeout_that_fills_the_whole_run_is_refused() {
    assert_eq!(
        TimeoutBudget::declare(1_200_000, 1_200_000, TierCaps::ATTENDED),
        Err(OperatorRefusal::MatrixTimeoutLeavesNoRoomForCleanup {
            per_matrix_ms: 1_200_000,
            whole_run_ms: 1_200_000,
        }),
        "a cap satisfiable only by skipping reversal and cleanup is not a cap"
    );
}

#[test]
fn a_timeout_looser_than_the_tier_cap_is_refused() {
    assert_eq!(
        TimeoutBudget::declare(5_400_001, 1_200_000, TierCaps::ATTENDED),
        Err(OperatorRefusal::TimeoutExceedsTierCap {
            declared_ms: 5_400_001,
            cap_ms: 5_400_000,
        }),
        "a plan may be tighter than the tier it runs under and never looser"
    );
}

#[test]
fn a_third_engine_makes_attribution_a_guess_and_is_refused() {
    assert_eq!(
        EngineProcessCount::declare(3, TierCaps::ATTENDED),
        Err(OperatorRefusal::TooManyEnginesForAttribution {
            declared: 3,
            cap: 2,
        }),
        "with a third listener the run cannot say which engine answered"
    );
}

#[test]
fn a_single_engine_is_not_a_differential_and_is_refused() {
    assert_eq!(
        EngineProcessCount::declare(1, TierCaps::ATTENDED),
        Err(OperatorRefusal::TooFewEnginesForDifferential {
            declared: 1,
            required: 2,
        }),
        "one process is a run with nothing to compare against: the ruling requires two the operator started, and a retained engine already running is not the second"
    );
}

#[test]
fn exactly_two_engine_processes_is_the_one_admitted_count() {
    let admitted = match EngineProcessCount::declare(2, TierCaps::ATTENDED) {
        Ok(count) => count,
        Err(refusal) => {
            panic!("the positive control between the two bounds must be admitted, got {refusal}")
        }
    };
    assert_eq!(
        admitted.get().get(),
        2,
        "the admitted count must be the count that was declared, not the bound it was compared with"
    );
}

/// The plan-level half of the engine law: a complete plan cannot carry one
/// engine, and no receipt can therefore report one.
///
/// The positive control is the same complete plan differing in exactly one
/// declaration, so this cannot pass by tripping the tier, boundary, mapping or
/// control guards — every one of those is satisfied here.
#[test]
fn a_complete_plan_can_neither_be_built_nor_receipted_with_one_engine() {
    let plan = match OperatorPlan::declare(sound_parts()) {
        Ok(plan) => plan,
        Err(refusal) => panic!(
            "the control for the one-engine witness must be a plan complete in every other declaration, refused with {refusal}"
        ),
    };
    let receipts = plan.component_receipts();
    let declared: Vec<&str> = receipts
        .iter()
        .filter(|receipt| receipt.component() == "engine-processes")
        .map(|receipt| receipt.declared())
        .collect();
    assert_eq!(
        declared,
        vec!["2"],
        "the one plan that admits must receipt exactly two engine processes, and exactly one row of them"
    );
    assert_eq!(
        EngineProcessCount::declare(1, TierCaps::ATTENDED),
        Err(OperatorRefusal::TooFewEnginesForDifferential {
            declared: 1,
            required: 2,
        }),
        "the count is the plan's ONLY route to an engine declaration, so a refusal here is what makes a one-engine plan unbuildable rather than merely undesirable"
    );
}

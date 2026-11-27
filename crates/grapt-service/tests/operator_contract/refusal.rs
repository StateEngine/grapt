// --------------------------------------------------------------------------
// The remaining reachable refusals, each witnessed once.
// --------------------------------------------------------------------------

#[test]
fn a_tier_with_no_caps_of_its_own_cannot_bound_a_live_run() {
    let mut parts = sound_parts();
    parts.tier = EvidenceTier::LocalFocused;
    assert_eq!(
        OperatorPlan::declare(parts),
        Err(OperatorRefusal::TierCarriesNoCapsOfItsOwn {
            tier: EvidenceTier::LocalFocused,
        }),
        "a live run has to be bounded by the tier it executes under, and the focused tier states no bounds"
    );
}

#[test]
fn an_unnamed_artifact_is_refused() {
    assert_eq!(
        DeclaredArtifact::declare("   ", ArtifactClass::RunReceipt, 1024),
        Err(OperatorRefusal::ArtifactUnnamed),
        "an artifact nobody can name cannot be pulled before cleanup removes it"
    );
}

#[test]
fn an_artifact_with_no_byte_budget_is_refused() {
    assert_eq!(
        DeclaredArtifact::declare("run-receipt.md", ArtifactClass::RunReceipt, 0),
        Err(OperatorRefusal::ArtifactBudgetUnbounded {
            name: "run-receipt.md".to_owned(),
        }),
        "a zero budget declares no bound at all while looking like one"
    );
}

#[test]
fn a_receipt_root_declared_without_both_paths_is_refused() {
    assert_eq!(
        ReceiptRoot::declare("C:/workspace/repo", "   "),
        Err(OperatorRefusal::ReceiptRootUnnamed),
        "the law is a relation between two paths and cannot be checked with one of them missing"
    );
}

#[test]
fn a_zero_whole_run_timeout_is_refused() {
    assert_eq!(
        TimeoutBudget::declare(0, 1_200_000, TierCaps::ATTENDED),
        Err(OperatorRefusal::RunTimeoutUnbounded),
        "an open-ended window on the fleet host is an outage nobody agreed to"
    );
}

#[test]
fn a_zero_per_matrix_timeout_is_refused() {
    assert_eq!(
        TimeoutBudget::declare(5_400_000, 0, TierCaps::ATTENDED),
        Err(OperatorRefusal::MatrixTimeoutUnbounded),
        "without a per-matrix bound no matrix can report which cap ended it"
    );
}

#[test]
fn declaring_no_engine_at_all_is_refused() {
    assert_eq!(
        EngineProcessCount::declare(0, TierCaps::ATTENDED),
        Err(OperatorRefusal::TooFewEnginesForDifferential {
            declared: 0,
            required: 2,
        }),
        "declaring nothing at all must be refused by the lower bound and must report the count it was given"
    );
}

#[test]
fn a_mapping_receipt_with_no_id_is_refused() {
    assert_eq!(
        RetainedOwnerMapping::declare(MappingReceipt::record(
            "  ",
            RetainedSource::RegistrationFilePath,
            RetainedSource::RegistrationId,
            RetainedSource::EnvironmentSidecarDigest,
            true,
            true,
            MappingBasis::DeclaredByOperator,
        )),
        Err(OperatorRefusal::MappingReceiptUnnamed),
        "every later refusal quotes the receipt, so a receipt that names nothing is judged first"
    );
}

#[test]
fn cleanup_may_remove_only_what_the_run_recorded_creating() {
    let inventory = CleanupInventory::enumerate(vec![
        ObservedPath::new("state/service-run.json", true),
        ObservedPath::new("state/service-other-seat.json", false),
        ObservedPath::new("state/service-.json.stale-10208", false),
    ]);
    match inventory.admit_deletion("state/service-run.json") {
        Ok(()) => {}
        Err(refusal) => panic!("the run's own file must be removable, got {refusal}"),
    }
    assert_eq!(
        inventory.admit_deletion("state/service-other-seat.json"),
        Err(OperatorRefusal::DeletionOfUnrecordedPath {
            path: "state/service-other-seat.json".to_owned(),
        }),
        "the state home is shared with live seats and another seat's file is recorded and left"
    );
    assert_eq!(
        inventory.removable().len(),
        1,
        "only the one path recorded as this run's may be removed, however many were observed"
    );
}

#[test]
fn a_leftover_with_no_constructor_in_the_frozen_tree_is_still_enumerated() {
    let inventory = CleanupInventory::enumerate(vec![ObservedPath::new(
        "state/service-.json.stale-10208",
        false,
    )]);
    assert_eq!(
        inventory.observed().len(),
        1,
        "a cleanup list derived from source would have missed this file entirely, which is why enumeration is the contract"
    );
    assert_eq!(
        inventory.removable().len(),
        0,
        "an observed file this run did not create is recorded and left, never tidied away"
    );
}

// --------------------------------------------------------------------------
// Vocabulary completeness.
// --------------------------------------------------------------------------

#[test]
fn every_own_label_is_produced_by_a_real_variant_and_the_array_has_no_gaps() {
    assert_eq!(
        OperatorRefusal::OWN_LABELS.len(),
        OperatorRefusal::OWN_LABEL_COUNT,
        "the declared count and the array must describe one vocabulary"
    );
    let mut sorted = OperatorRefusal::OWN_LABELS.to_vec();
    sorted.sort_unstable();
    let before = sorted.len();
    sorted.dedup();
    assert_eq!(
        sorted.len(),
        before,
        "one label appearing twice would let two laws share a corpus row"
    );
}

#[test]
fn a_landed_refusal_carried_by_this_lane_keeps_its_own_word() {
    let carried = OperatorRefusal::from(grapt_service::DifferentialRefusal::LeavesSelectorAttached);
    assert_eq!(
        carried.label(),
        "leaves-selector-attached",
        "a landed law must reach this lane's projection in the words the lane that owns it chose"
    );
    assert_eq!(
        carried.own_label_index(),
        None,
        "a carried landed refusal has no index in this lane's own vocabulary, which is what keeps the two disjoint"
    );
}

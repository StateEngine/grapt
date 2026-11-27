use super::*;

// ---------------------------------------------------------------------------
// Subset completeness
// ---------------------------------------------------------------------------

#[test]
fn a_matrix_measured_below_its_minimum_subset_is_refused_naming_every_missing_operation() {
    // The differential matrix needs a listing as well as health. Health alone
    // compares two liveness answers and calls the result parity.
    expect_refusal(
        OperationSubset::declare(DifferentialMatrix::Differential, vec![Operation::HealthGet]),
        &DifferentialRefusal::MatrixSubsetIncomplete {
            matrix: DifferentialMatrix::Differential,
            missing: vec![Operation::SessionList],
        },
        "a narrowed differential subset",
    );

    // And the reverse omission, so the check is not passing on one hard-coded
    // operation: dropping health from sleep/resume must name health.
    expect_refusal(
        OperationSubset::declare(
            DifferentialMatrix::SleepResume,
            vec![Operation::SessionList],
        ),
        &DifferentialRefusal::MatrixSubsetIncomplete {
            matrix: DifferentialMatrix::SleepResume,
            missing: vec![Operation::HealthGet],
        },
        "a resume subset without its re-admission probe",
    );
}

#[test]
fn every_matrix_admits_exactly_its_own_pinned_minimum_subset() {
    for matrix in DifferentialMatrix::ALL {
        let declared = subset(matrix);
        assert_eq!(
            declared.operations(),
            matrix.minimum_subset(),
            "{}: the pinned minimum must be declarable as-is",
            matrix.label()
        );
        assert!(
            !matrix.minimum_subset().is_empty(),
            "{}: a matrix with an empty minimum subset would admit a run that measured nothing",
            matrix.label()
        );
    }
}

#[test]
fn a_wider_subset_is_admitted_because_measuring_more_is_never_a_false_claim() {
    let wider = OperationSubset::declare(
        DifferentialMatrix::Restart,
        vec![Operation::HealthGet, Operation::SessionList],
    );
    match wider {
        Ok(subset) => assert_eq!(subset.operations().len(), 2),
        Err(refusal) => panic!("a superset of the minimum must be admitted: {refusal}"),
    }
}

#[test]
fn an_operation_declared_twice_is_refused_before_any_omission_is_reported() {
    // This list is BOTH duplicated and (counting distinct operations) short of
    // the differential minimum. The duplicate must be named: reporting only the
    // omission would hide why the list was wrong.
    expect_refusal(
        OperationSubset::declare(
            DifferentialMatrix::Differential,
            vec![Operation::HealthGet, Operation::HealthGet],
        ),
        &DifferentialRefusal::OperationDeclaredTwice {
            matrix: DifferentialMatrix::Differential,
            operation: Operation::HealthGet,
        },
        "a subset holding one operation twice",
    );
}

#[test]
fn every_operation_id_round_trips_and_unknown_text_is_refused_rather_than_defaulted() {
    for operation in Operation::ALL {
        assert_eq!(
            Operation::from_operation_id(operation.operation_id()),
            Some(operation),
            "an operation id must name the operation it came from"
        );
    }
    assert_eq!(
        Operation::from_operation_id("v2.session.get"),
        None,
        "an operation this contract does not own must be refused, never defaulted to one it does"
    );
    // Pinned against the frozen surface rather than against this enum.
    assert_eq!(Operation::HealthGet.operation_id(), "v2.health.get");
    assert_eq!(Operation::SessionList.operation_id(), "v2.session.list");
}

// ---------------------------------------------------------------------------
// Recorded host bounds
// ---------------------------------------------------------------------------

#[test]
fn a_bound_the_driver_asserted_is_refused_naming_the_bound_and_its_source() {
    for inadmissible in [
        BoundProvenance::AssertedByDriver,
        BoundProvenance::DefaultedByDriver,
    ] {
        let mut bounds = complete_bounds();
        replace_bound(
            &mut bounds,
            HostBoundName::MaxRequestHeadBytes,
            RecordedBound::new(HostBoundName::MaxRequestHeadBytes, "8192", inadmissible),
        );
        expect_refusal(
            RecordedHostBounds::record(bounds),
            &DifferentialRefusal::BoundNotReportedByHost {
                name: HostBoundName::MaxRequestHeadBytes,
                provenance: inadmissible,
            },
            "a driver-authored bound",
        );
    }
}

#[test]
fn an_incomplete_recording_is_refused_naming_every_bound_it_omitted() {
    let bounds: Vec<RecordedBound> = complete_bounds()
        .into_iter()
        .filter(|bound| {
            bound.name() != HostBoundName::MaxHeaders
                && bound.name() != HostBoundName::ShutdownDrainAllowance
        })
        .collect();
    expect_refusal(
        RecordedHostBounds::record(bounds),
        &DifferentialRefusal::BoundSetIncomplete {
            missing: vec![
                HostBoundName::MaxHeaders,
                HostBoundName::ShutdownDrainAllowance,
            ],
        },
        "a recording missing two required bounds",
    );
}

#[test]
fn a_bound_recorded_twice_is_refused_even_though_the_count_still_looks_complete() {
    // A complete count, all host-sourced, none empty — a count-only check passes
    // this. One bound stands in for another, which is exactly the substitution
    // the law forbids.
    let mut bounds = complete_bounds();
    replace_bound(
        &mut bounds,
        HostBoundName::ShutdownDrainAllowance,
        RecordedBound::new(
            HostBoundName::MaxConnections,
            "recorded-by-host",
            BoundProvenance::ReportedByHostReceipt,
        ),
    );
    assert_eq!(
        bounds.len(),
        HostBoundName::ALL.len(),
        "the duplicate case must keep a complete count"
    );
    expect_refusal(
        RecordedHostBounds::record(bounds),
        &DifferentialRefusal::BoundRecordedTwice {
            name: HostBoundName::MaxConnections,
        },
        "a bound recorded twice",
    );
}

#[test]
fn an_empty_rendering_is_refused_because_it_records_nothing_while_looking_like_a_reading() {
    let mut bounds = complete_bounds();
    replace_bound(
        &mut bounds,
        HostBoundName::MaxConnections,
        RecordedBound::new(
            HostBoundName::MaxConnections,
            "",
            BoundProvenance::ReportedByHostReceipt,
        ),
    );
    expect_refusal(
        RecordedHostBounds::record(bounds),
        &DifferentialRefusal::BoundRenderingEmpty {
            name: HostBoundName::MaxConnections,
        },
        "a bound with an empty rendering",
    );
}

fn replace_bound(bounds: &mut [RecordedBound], name: HostBoundName, replacement: RecordedBound) {
    let Some(bound) = bounds.iter_mut().find(|bound| bound.name() == name) else {
        panic!("fixture omitted required bound {name:?}");
    };
    *bound = replacement;
}

#[test]
fn a_recording_is_stored_in_a_fixed_order_regardless_of_the_order_it_was_read_in() {
    let mut reversed = complete_bounds();
    reversed.reverse();
    let recorded = match RecordedHostBounds::record(reversed) {
        Ok(recorded) => recorded,
        Err(refusal) => panic!("a complete recording must admit in any order: {refusal}"),
    };
    let order: Vec<HostBoundName> = recorded.bounds().iter().map(RecordedBound::name).collect();
    assert_eq!(
        order,
        HostBoundName::ALL.to_vec(),
        "two drivers reading the same host in different orders must project identically"
    );
}

#[test]
fn every_bound_field_name_round_trips_and_an_unknown_field_is_refused() {
    for name in HostBoundName::ALL {
        assert_eq!(
            HostBoundName::from_field_name(name.field_name()),
            Some(name),
            "a field name must name the bound it came from"
        );
    }
    assert_eq!(
        HostBoundName::from_field_name("max_connection"),
        None,
        "a near-miss field name must be refused, not resolved to its neighbour"
    );
    assert_eq!(
        HostBoundName::ALL.len(),
        10,
        "another bound is a reviewed law change here and a red drift gate, never a silent omission"
    );
    assert!(
        HostBoundName::ALL.contains(&HostBoundName::StreamWriteAllowance),
        "the host bound inventory must include the stream write-stall allowance"
    );
    assert!(
        HostBoundName::ALL.contains(&HostBoundName::MaxInflightStreams),
        "the host bound inventory must include the separate event stream capacity"
    );
}

// --------------------------------------------------------------------------
// Phase gating — the parked half, which is all an external caller can reach.
// --------------------------------------------------------------------------

fn parked_gate() -> PhaseGate {
    PhaseGate::open(RunAuthorization::Parked(
        ParkReason::AttendedWindowNotScheduled,
    ))
}

#[test]
fn a_parked_plan_may_be_announced_and_may_go_no_further() {
    let mut gate = parked_gate();
    match gate.enter(RunPhase::Announce) {
        Ok(()) => {}
        Err(refusal) => panic!(
            "announcing a parked plan is how the park is published and must be permitted, got {refusal}"
        ),
    }
    assert_eq!(
        gate.enter(RunPhase::RecordHostStateAtStart),
        Err(OperatorRefusal::ExecutionNotAuthorized {
            phase: RunPhase::RecordHostStateAtStart,
            park_reason: Some(ParkReason::AttendedWindowNotScheduled),
        }),
        "the tier is declared and its execution is withheld, so the first executable phase is where that becomes mechanical"
    );
    assert_eq!(
        gate.conclude(),
        Ok(GateConclusion::ParkedAtAnnouncement),
        "parked at the announcement is the correct terminal state for every plan this crate can build today"
    );
}

#[test]
fn a_phase_entered_out_of_order_names_the_phase_that_was_expected() {
    let mut gate = parked_gate();
    assert_eq!(
        gate.enter(RunPhase::StopEngines),
        Err(OperatorRefusal::PhaseOutOfOrder {
            attempted: RunPhase::StopEngines,
            expected: RunPhase::Announce,
        }),
        "order is checked before authority so a structurally wrong run names the structure first"
    );
}

#[test]
fn a_run_concluded_without_an_announcement_is_refused() {
    assert_eq!(
        parked_gate().conclude(),
        Err(OperatorRefusal::RunNeverAnnounced),
        "an attended window is a custody claim and an unannounced run cannot make it"
    );
}

// --------------------------------------------------------------------------
// Negative controls.
// --------------------------------------------------------------------------

#[test]
fn a_plan_omitting_a_negative_control_is_refused_naming_every_missing_one() {
    let mut parts = sound_parts();
    parts.controls = vec![
        NegativeControl::BothSidesOneEngine,
        NegativeControl::ProbeNamedNoOwner,
        NegativeControl::ReceiptReusedAfterReplacement,
        NegativeControl::SubsetMissingRequiredOperation,
    ];
    assert_eq!(
        OperatorPlan::declare(parts),
        Err(OperatorRefusal::NegativeControlOmitted {
            missing: vec![
                NegativeControl::DriverSuppliedBound,
                NegativeControl::SelectorLeftAttached,
            ],
        }),
        "a run that only produces greens has not tested its instrument, and every omission is reported at once"
    );
}

// --------------------------------------------------------------------------
// The pre-entrypoint construction refusal — this lane's centrepiece.
// --------------------------------------------------------------------------

#[test]
fn no_differential_plan_is_constructible_while_no_host_has_reported_its_bounds() {
    let plan = match OperatorPlan::declare(sound_parts()) {
        Ok(plan) => plan,
        Err(refusal) => panic!("setup for the pre-entrypoint witness failed with {refusal}"),
    };
    let subset = match grapt_service::OperationSubset::declare(
        DifferentialMatrix::Restart,
        vec![Operation::HealthGet],
    ) {
        Ok(subset) => subset,
        Err(refusal) => panic!("the landed restart subset must declare, got {refusal}"),
    };
    let refusal = construct_differential_plan(
        &plan,
        grapt_service::DifferentialConstruction {
            bounds: HostBoundsSource::NoHostHasRun,
            subset,
            engines: engine_pair(),
            evidence: grapt_service::MatrixEvidence::None,
            terminal_selector: grapt_service::SelectorState::Retained,
        },
    );
    assert_eq!(
        refusal,
        Err(OperatorRefusal::HostBoundsUnavailableBeforeEntrypoint),
        "a recording admits only host-reported provenance and there is no ready host to read one from, so the seam must stop here"
    );
}

/// The absent-entrypoint case is refused at DECLARATION, not at construction.
///
/// A plan naming `NoHostRuns` cannot be built at all, because that entrypoint
/// serves no operation and every matrix's landed minimum subset therefore goes
/// unmet. That is why `construct_differential_plan` carries no second
/// "entrypoint absent" refusal: it could never have fired.
#[test]
fn the_absent_entrypoint_is_refused_where_it_is_actually_reachable() {
    for matrix in [
        DifferentialMatrix::Differential,
        DifferentialMatrix::Restart,
        DifferentialMatrix::LongSession,
    ] {
        let mut parts = sound_parts();
        parts.entrypoint = EntrypointCapability::NoHostRuns;
        parts.matrix = matrix;
        parts.boundary = match matrix {
            DifferentialMatrix::Restart => BoundaryKind::EngineRestart,
            _ => BoundaryKind::None,
        };
        match OperatorPlan::declare(parts) {
            Err(OperatorRefusal::MatrixUnservableAtEntrypoint { .. }) => {}
            other => panic!(
                "with no host running, the {} matrix must be refused as unservable at declaration; got {other:?}",
                matrix.label()
            ),
        }
    }
}

/// Two admitted engine receipts, built from literals in this file.
fn engine_pair() -> grapt_service::AdmittedDifferential {
    use grapt_service::{EngineSelection, ProbeOutcome, SelectionReceipt};
    use grapt_types::authority::{ClockAuthority, Timestamp};
    use grapt_types::service::{
        Endpoint, EnvironmentReceipt, OwnerReceipt, ProcessId, ServiceGeneration,
        ServiceInstanceId, ServiceSlotId,
    };

    #[derive(Debug)]
    struct FixedClock(i64);
    impl ClockAuthority for FixedClock {
        fn now(&self) -> Timestamp {
            Timestamp::from_millis(self.0)
        }
    }

    fn owner(slot: &str, instance: &str, environment: &str, pid: u32, url: &str) -> OwnerReceipt {
        let parts = (
            ServiceSlotId::new(slot),
            ServiceInstanceId::new(instance),
            EnvironmentReceipt::new(environment),
            ProcessId::new(pid),
            Endpoint::new(url),
        );
        match parts {
            (Some(slot), Some(instance), Some(environment), Some(pid), Some(endpoint)) => {
                OwnerReceipt::new(
                    slot,
                    instance,
                    environment,
                    ServiceGeneration::FIRST,
                    pid,
                    endpoint,
                )
            }
            _ => panic!("the literal owner coordinates in this witness must all be non-empty"),
        }
    }

    let clock = FixedClock(1_760_000_000_000);
    let left = match SelectionReceipt::admit(
        EngineSelection::Rust,
        ProbeOutcome::Attributed(owner(
            "slot-rust",
            "instance-rust",
            "env-rust",
            4321,
            "http://127.0.0.1:4321",
        )),
        &clock,
    ) {
        Ok(receipt) => receipt,
        Err(refusal) => panic!("the rust-side witness receipt must admit, got {refusal}"),
    };
    let right = match SelectionReceipt::admit(
        EngineSelection::Retained,
        ProbeOutcome::Attributed(owner(
            "slot-retained",
            "instance-retained",
            "env-retained",
            8765,
            "http://127.0.0.1:8765",
        )),
        &clock,
    ) {
        Ok(receipt) => receipt,
        Err(refusal) => panic!("the retained-side witness receipt must admit, got {refusal}"),
    };
    match grapt_service::AdmittedDifferential::admit(left, right) {
        Ok(pair) => pair,
        Err(refusal) => panic!("two distinct engines must admit as a pair, got {refusal}"),
    }
}

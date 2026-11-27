// --------------------------------------------------------------------------
// The positive control for the whole plan.
// --------------------------------------------------------------------------

#[test]
fn the_restart_scoped_plan_this_lane_recommends_is_admitted() {
    let plan = match OperatorPlan::declare(sound_parts()) {
        Ok(plan) => plan,
        Err(refusal) => panic!(
            "the restart-scoped plan is the one shape this lane says is declarable today, refused with {refusal}"
        ),
    };
    assert_eq!(
        plan.matrix(),
        DifferentialMatrix::Restart,
        "the only matrix a one-route entrypoint can serve must survive declaration"
    );
    assert_eq!(
        plan.component_receipts().len(),
        12,
        "every component must contribute exactly one receipt, or the run receipt under-reports what was declared"
    );
    assert_eq!(
        plan.kill().label(),
        "pid-scoped",
        "the declared plan must retain the admitted pid-scoped kill"
    );
    assert_eq!(
        plan.comparison(),
        ComparisonScope::ShapeAndStatusOnly,
        "the declared plan must retain the admitted comparison scope"
    );
    assert_eq!(
        KillDiscipline::PidScoped(4321).admit(),
        Ok(KillDiscipline::PidScoped(4321)),
        "admitting an already-admissible pid-scoped kill must be idempotent"
    );
}

#[test]
fn a_plan_declared_with_an_unadmitted_image_name_kill_is_refused() {
    let mut parts = sound_parts();
    parts.kill = KillDiscipline::ByImageName("node.exe".to_owned());
    assert_eq!(
        OperatorPlan::declare(parts),
        Err(OperatorRefusal::ImageNameKillRefused {
            image: "node.exe".to_owned(),
        }),
        "the plan seam must refuse an image-name kill even when its caller omitted admission"
    );
}

#[test]
fn a_plan_declared_with_a_zero_pid_kill_is_refused() {
    let mut parts = sound_parts();
    parts.kill = KillDiscipline::PidScoped(0);
    assert_eq!(
        OperatorPlan::declare(parts),
        Err(OperatorRefusal::KillTargetNotAProcess),
        "the plan seam must refuse a zero pid even when its caller omitted admission"
    );
}

#[test]
fn a_plan_declaring_a_body_content_comparison_is_refused_without_a_proof() {
    let mut parts = sound_parts();
    parts.comparison = ComparisonScope::BodyContent;
    assert_eq!(
        OperatorPlan::declare(parts),
        Err(OperatorRefusal::BodyContentComparisonUnproven),
        "the plan seam must refuse body-content comparison until a common-store owner can supply its sealed proof"
    );
}

// --------------------------------------------------------------------------
// R-Z1 — the entrypoint cannot serve three of the four matrices.
// --------------------------------------------------------------------------

#[test]
fn the_differential_matrix_is_unservable_at_a_one_route_entrypoint() {
    let mut parts = sound_parts();
    parts.matrix = DifferentialMatrix::Differential;
    parts.boundary = BoundaryKind::None;
    assert_eq!(
        OperatorPlan::declare(parts),
        Err(OperatorRefusal::MatrixUnservableAtEntrypoint {
            matrix: DifferentialMatrix::Differential,
            entrypoint: EntrypointCapability::ExactlyOneHealthRoute,
            missing: vec![Operation::SessionList],
        }),
        "the landed differential subset names the session listing, which a one-route entrypoint cannot serve"
    );
}

#[test]
fn the_long_session_matrix_is_unservable_at_a_one_route_entrypoint() {
    let mut parts = sound_parts();
    parts.matrix = DifferentialMatrix::LongSession;
    parts.boundary = BoundaryKind::None;
    assert_eq!(
        OperatorPlan::declare(parts),
        Err(OperatorRefusal::MatrixUnservableAtEntrypoint {
            matrix: DifferentialMatrix::LongSession,
            entrypoint: EntrypointCapability::ExactlyOneHealthRoute,
            missing: vec![Operation::SessionList],
        }),
        "the long-session matrix has no operation at all at a health-only entrypoint"
    );
}

#[test]
fn a_matrix_is_unservable_when_no_host_runs_at_all() {
    let mut parts = sound_parts();
    parts.entrypoint = EntrypointCapability::NoHostRuns;
    assert_eq!(
        OperatorPlan::declare(parts),
        Err(OperatorRefusal::MatrixUnservableAtEntrypoint {
            matrix: DifferentialMatrix::Restart,
            entrypoint: EntrypointCapability::NoHostRuns,
            missing: vec![Operation::HealthGet],
        }),
        "even the restart matrix needs the health probe, and at this canonical nothing serves it"
    );
}

#[test]
fn the_contradiction_dissolves_once_a_session_route_is_served() {
    let mut parts = sound_parts();
    parts.entrypoint = EntrypointCapability::HealthAndSessionList;
    parts.matrix = DifferentialMatrix::Differential;
    parts.boundary = BoundaryKind::None;
    match OperatorPlan::declare(parts) {
        Ok(plan) => assert_eq!(
            plan.matrix(),
            DifferentialMatrix::Differential,
            "with a session route the differential matrix becomes declarable with no subset amended"
        ),
        Err(refusal) => panic!(
            "a session route must make the differential matrix declarable without touching a landed subset, refused with {refusal}"
        ),
    }
}

// --------------------------------------------------------------------------
// R-Z3 — sleep/resume is parked, and no substitute stands in for it.
// --------------------------------------------------------------------------

#[test]
fn the_sleep_resume_matrix_is_parked() {
    let mut parts = sound_parts();
    parts.matrix = DifferentialMatrix::SleepResume;
    parts.boundary = BoundaryKind::HostSleep;
    assert_eq!(
        OperatorPlan::declare(parts),
        Err(OperatorRefusal::SleepResumeParked),
        "a real host sleep suspends every seat on this machine, and that decision is not the operator's to take"
    );
}

#[test]
fn a_process_suspend_offered_as_a_host_sleep_names_the_substitute_not_the_park() {
    let mut parts = sound_parts();
    parts.matrix = DifferentialMatrix::SleepResume;
    parts.boundary = BoundaryKind::ProcessSuspend;
    assert_eq!(
        OperatorPlan::declare(parts),
        Err(OperatorRefusal::SubstituteBoundaryRefused {
            offered: BoundaryKind::ProcessSuspend,
        }),
        "the substitute is the more specific defect and must be reported ahead of the park that would otherwise hide it"
    );
}

#[test]
fn a_network_drop_offered_as_a_host_sleep_is_refused_as_a_substitute() {
    let mut parts = sound_parts();
    parts.matrix = DifferentialMatrix::SleepResume;
    parts.boundary = BoundaryKind::NetworkDrop;
    assert_eq!(
        OperatorPlan::declare(parts),
        Err(OperatorRefusal::SubstituteBoundaryRefused {
            offered: BoundaryKind::NetworkDrop,
        }),
        "dropping the network is a different matrix wearing the sleep matrix's label"
    );
}

#[test]
fn a_restart_matrix_run_across_no_boundary_is_refused() {
    let mut parts = sound_parts();
    parts.boundary = BoundaryKind::None;
    assert_eq!(
        OperatorPlan::declare(parts),
        Err(OperatorRefusal::BoundaryDoesNotMatchMatrix {
            matrix: DifferentialMatrix::Restart,
            offered: BoundaryKind::None,
            required: BoundaryKind::EngineRestart,
        }),
        "a restart matrix that crossed no boundary measured something other than a restart"
    );
}

// --------------------------------------------------------------------------
// R-Z4 — the retained mapping is declared, fails closed, and never takes an
// attribute for an identity.
// --------------------------------------------------------------------------

#[test]
fn a_plan_without_a_retained_mapping_receipt_fails_closed() {
    let mut parts = sound_parts();
    parts.mapping = None;
    assert_eq!(
        OperatorPlan::declare(parts),
        Err(OperatorRefusal::RetainedMappingReceiptAbsent),
        "with no declared mapping the retained side would be attributed by guesswork, so the run stops instead"
    );
}

#[test]
fn a_pid_offered_as_the_instance_coordinate_is_refused() {
    assert_eq!(
        RetainedOwnerMapping::declare(mapping_receipt(
            RetainedSource::RegistrationFilePath,
            RetainedSource::ProcessIdentifier,
            RetainedSource::EnvironmentSidecarDigest,
            true,
            true,
            MappingBasis::DeclaredByOperator,
        )),
        Err(OperatorRefusal::PidOfferedAsIdentity {
            coordinate: OwnerCoordinate::Instance,
        }),
        "pid is an attribute under D1 and a reused pid must never inherit ownership"
    );
}

#[test]
fn an_endpoint_offered_as_the_slot_coordinate_is_refused() {
    assert_eq!(
        RetainedOwnerMapping::declare(mapping_receipt(
            RetainedSource::Endpoint,
            RetainedSource::RegistrationId,
            RetainedSource::EnvironmentSidecarDigest,
            true,
            true,
            MappingBasis::DeclaredByOperator,
        )),
        Err(OperatorRefusal::EndpointOfferedAsIdentity {
            coordinate: OwnerCoordinate::Slot,
        }),
        "one owner keeps its slot across a new ephemeral port, so the endpoint cannot be the slot"
    );
}

#[test]
fn a_version_offered_as_the_environment_coordinate_is_refused() {
    assert_eq!(
        RetainedOwnerMapping::declare(mapping_receipt(
            RetainedSource::RegistrationFilePath,
            RetainedSource::RegistrationId,
            RetainedSource::Version,
            true,
            true,
            MappingBasis::DeclaredByOperator,
        )),
        Err(OperatorRefusal::VersionOfferedAsIdentity {
            coordinate: OwnerCoordinate::Environment,
        }),
        "two independent engines can report one version, so a version identifies no environment"
    );
}

#[test]
fn a_crossed_mapping_whose_every_source_is_identity_bearing_is_still_refused() {
    assert_eq!(
        RetainedOwnerMapping::declare(mapping_receipt(
            RetainedSource::EnvironmentSidecarDigest,
            RetainedSource::RegistrationId,
            RetainedSource::RegistrationFilePath,
            true,
            true,
            MappingBasis::DeclaredByOperator,
        )),
        Err(OperatorRefusal::CoordinateSourceMismatch {
            coordinate: OwnerCoordinate::Slot,
            offered: RetainedSource::EnvironmentSidecarDigest,
        }),
        "no per-source check can see this: every source involved is identity-bearing and only the pairing is wrong"
    );
}

#[test]
fn a_registration_without_an_id_stops_rather_than_synthesizing_an_instance() {
    assert_eq!(
        RetainedOwnerMapping::declare(mapping_receipt(
            RetainedSource::RegistrationFilePath,
            RetainedSource::RegistrationId,
            RetainedSource::EnvironmentSidecarDigest,
            false,
            true,
            MappingBasis::DeclaredByOperator,
        )),
        Err(OperatorRefusal::RetainedInstanceIdentityAbsent),
        "the id field is optional in the retained schema, and absent it there is no instance identity to map"
    );
}

#[test]
fn an_unpinned_state_home_leaves_the_retained_slot_ambiguous() {
    assert_eq!(
        RetainedOwnerMapping::declare(mapping_receipt(
            RetainedSource::RegistrationFilePath,
            RetainedSource::RegistrationId,
            RetainedSource::EnvironmentSidecarDigest,
            true,
            false,
            MappingBasis::DeclaredByOperator,
        )),
        Err(OperatorRefusal::RetainedSlotAmbiguous),
        "four registration files were observed in one state home and readdir order would pick the winner"
    );
}

#[test]
fn a_mapping_claiming_to_be_read_from_the_wire_is_refused_before_its_coordinates() {
    assert_eq!(
        RetainedOwnerMapping::declare(mapping_receipt(
            RetainedSource::ProcessIdentifier,
            RetainedSource::ProcessIdentifier,
            RetainedSource::ProcessIdentifier,
            false,
            false,
            MappingBasis::ReadFromWire,
        )),
        Err(OperatorRefusal::MappingReadFromWire),
        "if it was read off the wire then which values it read is beside the point, so the basis is judged first"
    );
}

// --------------------------------------------------------------------------
// R-Z5 — comparison is shape and status only.
// --------------------------------------------------------------------------

#[test]
fn a_body_content_comparison_is_refused_without_a_store_equivalence_proof() {
    assert_eq!(
        ComparisonScope::BodyContent.admit(None),
        Err(OperatorRefusal::BodyContentComparisonUnproven),
        "two engines reading different data roots differ in every row, and that is a data difference not an engine one"
    );
}

#[test]
fn a_shape_and_status_comparison_needs_no_proof() {
    assert_eq!(
        ComparisonScope::ShapeAndStatusOnly.admit(None),
        Ok(ComparisonScope::ShapeAndStatusOnly),
        "shape and status is the scope this lane is ruled to, so it must be declarable with nothing extra"
    );
}

#[test]
fn an_observation_without_a_body_digest_is_refused() {
    assert_eq!(
        ShapeObservation::record("v2.health.get", 200, vec!["content-type".to_owned()], "", 0),
        Err(OperatorRefusal::ObservationDigestAbsent {
            operation_id: "v2.health.get".to_owned(),
        }),
        "an observation with no digest records that a request happened and nothing about the answer"
    );
}

#[test]
fn an_observation_records_shape_and_status_and_has_nowhere_to_put_a_body() {
    match ShapeObservation::record(
        "v2.health.get",
        200,
        vec!["content-type".to_owned()],
        "sha256:unmeasured",
        42,
    ) {
        Ok(observation) => {
            assert_eq!(observation.status(), 200);
            assert_eq!(
                observation.body_digest(),
                "sha256:unmeasured",
                "the digest compares two answers without carrying either of them"
            );
            assert_eq!(
                observation.body_bytes(),
                42,
                "a byte count is accounting, not content"
            );
        }
        Err(refusal) => panic!("the sound observation must be recorded, got {refusal}"),
    }
}

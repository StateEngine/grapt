//! R4D-2E — the operator contract's laws, each witnessed against its own
//! refusal.
//!
//! Every refusal case here has a positive control that differs from it in
//! exactly one declaration, so no law can pass by tripping another's guard. Each
//! assertion message names the divergence it is asserting rather than restating
//! the values: two witnesses whose failure lines were identical would read as
//! one law witnessed twice in the mutation battery's evidence, and the values in
//! an `assert_eq!` do not reach the panic line.
//!
//! Nothing here starts a process, opens a socket, reads a clock or touches a
//! filesystem. Every value is constructed from a literal in this file.

use grapt_service::{
    construct_differential_plan, ArtifactClass, ArtifactCustody, BoundaryKind, CleanupInventory,
    ComparisonScope, DeclaredArtifact, DifferentialMatrix, EngineProcessCount,
    EntrypointCapability, EvidenceTier, GateConclusion, HostBoundsSource, KillDiscipline,
    MappingBasis, MappingReceipt, NegativeControl, ObservedPath, Operation, OperatorPlan,
    OperatorPlanParts, OperatorRefusal, OwnerCoordinate, ParkReason, PhaseGate, ReceiptRoot,
    RetainedOwnerMapping, RetainedSource, RunAuthorization, RunPhase, ShapeObservation, TierCaps,
    TimeoutBudget,
};

// --------------------------------------------------------------------------
// Builders. Each returns the SOUND value; every witness below derives its
// defective input from one of these by changing exactly one thing.
// --------------------------------------------------------------------------

fn sound_custody() -> ArtifactCustody {
    let root = match ReceiptRoot::declare("C:/workspace/repo", "C:/fixtures") {
        Ok(root) => root,
        Err(refusal) => panic!("the sound receipt root must be admitted, got {refusal}"),
    };
    let artifacts = vec![
        declared("run-receipt.md", ArtifactClass::RunReceipt, 262_144),
        declared("owner-mapping.json", ArtifactClass::OwnerMapping, 65_536),
        declared(
            "observations.jsonl",
            ArtifactClass::ShapeAndStatusObservation,
            262_144,
        ),
        declared("cleanup.jsonl", ArtifactClass::CleanupEnumeration, 65_536),
    ];
    match ArtifactCustody::declare(root, artifacts) {
        Ok(custody) => custody,
        Err(refusal) => panic!("the sound custody must be admitted, got {refusal}"),
    }
}

fn declared(name: &str, class: ArtifactClass, bytes: u64) -> DeclaredArtifact {
    match DeclaredArtifact::declare(name, class, bytes) {
        Ok(artifact) => artifact,
        Err(refusal) => panic!("the sound artifact {name} must be admitted, got {refusal}"),
    }
}

fn sound_timeout() -> TimeoutBudget {
    match TimeoutBudget::declare(5_400_000, 1_200_000, TierCaps::ATTENDED) {
        Ok(budget) => budget,
        Err(refusal) => panic!("the sound timeout must be admitted, got {refusal}"),
    }
}

fn sound_engines() -> EngineProcessCount {
    match EngineProcessCount::declare(2, TierCaps::ATTENDED) {
        Ok(count) => count,
        Err(refusal) => panic!("the sound engine count must be admitted, got {refusal}"),
    }
}

fn sound_mapping() -> RetainedOwnerMapping {
    match RetainedOwnerMapping::declare(mapping_receipt(
        RetainedSource::RegistrationFilePath,
        RetainedSource::RegistrationId,
        RetainedSource::EnvironmentSidecarDigest,
        true,
        true,
        MappingBasis::DeclaredByOperator,
    )) {
        Ok(mapping) => mapping,
        Err(refusal) => panic!("the sound mapping must be admitted, got {refusal}"),
    }
}

fn mapping_receipt(
    slot: RetainedSource,
    instance: RetainedSource,
    environment: RetainedSource,
    carries_id: bool,
    pinned: bool,
    basis: MappingBasis,
) -> MappingReceipt {
    MappingReceipt::record(
        "mapping-r4d2e-01",
        slot,
        instance,
        environment,
        carries_id,
        pinned,
        basis,
    )
}

fn sound_parts() -> OperatorPlanParts {
    OperatorPlanParts {
        tier: EvidenceTier::AttendedFleetHost,
        authorization: RunAuthorization::Parked(ParkReason::AttendedWindowNotScheduled),
        entrypoint: EntrypointCapability::ExactlyOneHealthRoute,
        matrix: DifferentialMatrix::Restart,
        boundary: BoundaryKind::EngineRestart,
        custody: sound_custody(),
        timeout: sound_timeout(),
        engines: sound_engines(),
        kill: match KillDiscipline::PidScoped(4321).admit() {
            Ok(kill) => kill,
            Err(refusal) => panic!("the sound kill discipline must be admitted, got {refusal}"),
        },
        mapping: Some(sound_mapping()),
        comparison: ComparisonScope::ShapeAndStatusOnly,
        controls: NegativeControl::ALL.to_vec(),
    }
}

include!("operator_contract/plan.rs");
include!("operator_contract/capability.rs");
include!("operator_contract/phase.rs");
include!("operator_contract/refusal.rs");

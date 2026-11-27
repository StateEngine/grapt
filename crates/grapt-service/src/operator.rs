//! R4D-2E — the pre-entrypoint operator contract for a live differential run.
//!
//! # What this module is
//!
//! R4D-2A landed the differential *driver* contract: the matrices, their pinned
//! minimum operation subsets, the identity laws and the shape of the bound
//! recording. It stops exactly where it must — at the point where a driver would
//! need a running engine.
//!
//! This module is the half in front of that: what an operator must have declared
//! *before* a run may start, and what must be refused when the declaration is
//! wrong. It is the components, never the plan. Nothing here opens a socket,
//! starts a process, drives a desktop, reads a clock or touches a filesystem,
//! and — the load-bearing part — [`construct_differential_plan`] **refuses** to
//! produce a [`DifferentialPlan`] until a host has actually reported its bounds.
//!
//! # Why no plan is constructible yet, and why that is correct
//!
//! [`RecordedHostBounds`] admits only `BoundProvenance::ReportedByHostReceipt`;
//! a driver-asserted or driver-defaulted bound is a named refusal. There is no
//! host to read those bounds from at this canonical: the workspace declares no
//! binary target, the API crate serves no product route, and a production build
//! answers `401` by ruling. So a plan cannot be admitted, **by the contract's own
//! design rather than by omission**.
//!
//! Any pre-entrypoint artifact that claimed a plan would either be fabricating a
//! bound or have found a hole in the landed contract. This module makes the
//! first impossible and would make the second visible.
//!
//! # It derives the landed laws rather than restating them
//!
//! The one place this lane could most easily have duplicated a landed authority
//! is the entrypoint law. A matrix is servable at an entrypoint exactly when the
//! matrix's **landed** minimum subset is covered by what the entrypoint serves —
//! so [`EntrypointCapability::satisfies`] calls
//! [`DifferentialMatrix::minimum_subset`] and compares. It does not carry a
//! second copy of the subsets, and if R4D-2A ever amends one, this law follows
//! in the same motion instead of contradicting it.
//!
//! [`OperatorRefusal::Differential`] likewise carries R4D-2A's refusal verbatim,
//! exactly as that lane carries R4D-1's, so there is one vocabulary per fact
//! across all three lanes.

use thiserror::Error;

use crate::attribution::{ComparisonScope, OwnerCoordinate, RetainedOwnerMapping, RetainedSource};
use crate::bounds::RecordedHostBounds;
use crate::custody::{
    ArtifactClass, ArtifactCustody, EngineProcessCount, KillDiscipline, TimeoutBudget,
};
use crate::differential::{
    DifferentialMatrix, DifferentialPlan, DifferentialRefusal, MatrixEvidence, Operation,
    OperationSubset,
};
use crate::receipt::{AdmittedDifferential, SelectorState};
use crate::tier::{EvidenceTier, RunAuthorization, TierCaps};

mod capability;
mod phase;
mod plan;
mod refusal;

pub use capability::{BoundaryKind, EntrypointCapability, NegativeControl};
pub use phase::{ComponentReceipt, GateConclusion, PhaseGate, RunPhase};
pub use plan::{
    construct_differential_plan, DifferentialConstruction, HostBoundsSource, OperatorPlan,
    OperatorPlanParts,
};
pub use refusal::OperatorRefusal;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_restart_matrix_survives_a_one_route_entrypoint() {
        assert_eq!(
            EntrypointCapability::ExactlyOneHealthRoute.servable_matrices(),
            vec![DifferentialMatrix::Restart],
            "three of the four landed minimum subsets name the session listing, which a one-route entrypoint cannot serve"
        );
    }

    #[test]
    fn no_matrix_at_all_is_servable_before_a_host_runs() {
        assert_eq!(
            EntrypointCapability::NoHostRuns.servable_matrices(),
            Vec::new(),
            "an entrypoint that serves nothing satisfies no minimum subset"
        );
    }

    #[test]
    fn a_session_route_makes_every_matrix_servable() {
        assert_eq!(
            EntrypointCapability::HealthAndSessionList.servable_matrices(),
            DifferentialMatrix::ALL.to_vec(),
            "the contradiction dissolves the day a session route lands, without amending a subset"
        );
    }

    #[test]
    fn reversal_precedes_stopping_and_enumeration_precedes_the_closing_record() {
        assert!(
            RunPhase::RevertSelector.ordinal() < RunPhase::StopEngines.ordinal(),
            "reversal precedes diagnosis on every exit path"
        );
        assert!(
            RunPhase::EnumerateLeftovers.ordinal() < RunPhase::RecordHostStateAtStop.ordinal(),
            "leftovers are recorded before the run declares itself finished"
        );
    }

    #[test]
    fn every_negative_control_expects_a_word_the_landed_contract_can_actually_produce() {
        for control in NegativeControl::ALL {
            assert!(
                DifferentialRefusal::ALL_LABELS.contains(&control.expected_refusal_label()),
                "control {} expects {}, which the landed contract cannot produce -- a control that can never fire looks identical to one that passed",
                control.label(),
                control.expected_refusal_label()
            );
        }
    }

    #[test]
    fn the_six_controls_exercise_six_distinct_landed_refusals() {
        let mut expected: Vec<&str> = NegativeControl::ALL
            .into_iter()
            .map(NegativeControl::expected_refusal_label)
            .collect();
        expected.sort_unstable();
        let distinct = expected.len();
        expected.dedup();
        assert_eq!(
            expected.len(),
            distinct,
            "two controls sharing one expected refusal would witness one law twice and the other not at all"
        );
    }

    /// A gate opened on a scheduled window, reachable only from `src/`.
    ///
    /// Everything past the announcement needs an authorization that admits
    /// execution, and the only constructor for one is `cfg(test)` and
    /// `pub(crate)`. So the executable half of the phase law is witnessed here;
    /// an integration test could not reach it even to try.
    fn scheduled_gate() -> PhaseGate {
        PhaseGate::open(RunAuthorization::Scheduled(
            crate::tier::AttendedWindowReceipt::sealed_for_witness("witness-window"),
        ))
    }

    #[test]
    fn a_scheduled_run_that_enters_every_phase_in_order_concludes_with_its_cleanup() {
        let mut gate = scheduled_gate();
        for phase in RunPhase::ALL {
            if let Err(refusal) = gate.enter(phase) {
                panic!("the ordered scheduled control must reach every phase, refused at {} with {refusal}", phase.label());
            }
        }
        assert_eq!(
            gate.conclude(),
            Ok(GateConclusion::CompletedWithCleanup),
            "a run that entered every phase in order cleaned up, and must be able to say so"
        );
    }

    #[test]
    fn a_scheduled_run_that_stops_before_its_cleanup_is_refused_naming_where_it_stopped() {
        let mut gate = scheduled_gate();
        for phase in [
            RunPhase::Announce,
            RunPhase::RecordHostStateAtStart,
            RunPhase::StartEngines,
            RunPhase::AdmitAttachments,
            RunPhase::ExerciseSubset,
            RunPhase::RevertSelector,
            RunPhase::StopEngines,
        ] {
            if let Err(refusal) = gate.enter(phase) {
                panic!(
                    "setup for the early-stop witness failed at {} with {refusal}",
                    phase.label()
                );
            }
        }
        assert_eq!(
            gate.conclude(),
            Err(OperatorRefusal::CleanupPhasesNotReached {
                reached: RunPhase::StopEngines
            }),
            "a run that stopped engines and never enumerated leftovers left files on a shared host and must not read as complete"
        );
    }

    #[test]
    fn nothing_may_be_entered_after_the_runs_final_phase() {
        let mut gate = scheduled_gate();
        for phase in RunPhase::ALL {
            if let Err(refusal) = gate.enter(phase) {
                panic!(
                    "setup for the after-the-last witness failed at {} with {refusal}",
                    phase.label()
                );
            }
        }
        assert_eq!(
            gate.enter(RunPhase::Announce),
            Err(OperatorRefusal::PhaseAfterTheLast {
                phase: RunPhase::Announce
            }),
            "a completed run has no next phase, and saying so is more truthful than reporting a repeat"
        );
    }

    #[test]
    fn a_phase_entered_twice_before_the_run_ends_names_the_re_entry() {
        let mut gate = scheduled_gate();
        if let Err(refusal) = gate.enter(RunPhase::Announce) {
            panic!("setup for the re-entry witness failed with {refusal}");
        }
        assert_eq!(
            gate.enter(RunPhase::Announce),
            Err(OperatorRefusal::PhaseAlreadyEntered {
                phase: RunPhase::Announce
            }),
            "mid-run, a repeated phase is a repeat and not a run that has ended"
        );
    }

    /// One instance of every refusal this lane mints.
    ///
    /// Written out rather than derived, because there is no way to enumerate a
    /// data-carrying enum's variants at runtime — and this list is not the
    /// unchecked kind that burned the R1 closure review. It cannot silently fall
    /// behind the enum: `own_label_index`'s match is exhaustive, so a new
    /// variant is a compile error until it is given an index; the index must
    /// point inside `OWN_LABELS`, whose length is asserted below; and
    /// `every_own_variant_maps_to_its_own_label` requires this list to cover
    /// every index exactly once. A variant added and left out of this list makes
    /// that test fail on a missing index rather than passing quietly.
    #[expect(clippy::too_many_lines, reason = "legacy")]
    fn one_of_every_own_refusal() -> Vec<OperatorRefusal> {
        vec![
            OperatorRefusal::TierCarriesNoCapsOfItsOwn {
                tier: EvidenceTier::LocalFocused,
            },
            OperatorRefusal::ExecutionNotAuthorized {
                phase: RunPhase::StartEngines,
                park_reason: Some(crate::tier::ParkReason::AttendedWindowNotScheduled),
            },
            OperatorRefusal::MatrixUnservableAtEntrypoint {
                matrix: DifferentialMatrix::Differential,
                entrypoint: EntrypointCapability::ExactlyOneHealthRoute,
                missing: vec![Operation::SessionList],
            },
            OperatorRefusal::SleepResumeParked,
            OperatorRefusal::SubstituteBoundaryRefused {
                offered: BoundaryKind::ProcessSuspend,
            },
            OperatorRefusal::BoundaryDoesNotMatchMatrix {
                matrix: DifferentialMatrix::Restart,
                offered: BoundaryKind::None,
                required: BoundaryKind::EngineRestart,
            },
            OperatorRefusal::HostBoundsUnavailableBeforeEntrypoint,
            OperatorRefusal::PhaseAlreadyEntered {
                phase: RunPhase::Announce,
            },
            OperatorRefusal::PhaseOutOfOrder {
                attempted: RunPhase::StopEngines,
                expected: RunPhase::Announce,
            },
            OperatorRefusal::PhaseAfterTheLast {
                phase: RunPhase::Announce,
            },
            OperatorRefusal::RunNeverAnnounced,
            OperatorRefusal::CleanupPhasesNotReached {
                reached: RunPhase::StopEngines,
            },
            OperatorRefusal::NegativeControlOmitted {
                missing: vec![NegativeControl::DriverSuppliedBound],
            },
            OperatorRefusal::ArtifactUnnamed,
            OperatorRefusal::ArtifactClassNotRetainable {
                class: ArtifactClass::SessionBodyContent,
            },
            OperatorRefusal::ArtifactBudgetUnbounded {
                name: "a".to_owned(),
            },
            OperatorRefusal::ArtifactDeclaredTwice {
                name: "a".to_owned(),
            },
            OperatorRefusal::ReceiptRootUnnamed,
            OperatorRefusal::ReceiptRootInsideRepository {
                receipt_root: "a".to_owned(),
            },
            OperatorRefusal::CustodyRetainsNothing,
            OperatorRefusal::CustodyBudgetExceeded {
                declared: 2,
                ceiling: 1,
            },
            OperatorRefusal::CustodyBudgetNotRepresentable {
                name: "a".to_owned(),
                preceding: u64::MAX,
            },
            OperatorRefusal::ImageNameKillRefused {
                image: "node.exe".to_owned(),
            },
            OperatorRefusal::KillTargetNotAProcess,
            OperatorRefusal::DeletionOfUnrecordedPath {
                path: "a".to_owned(),
            },
            OperatorRefusal::RunTimeoutUnbounded,
            OperatorRefusal::MatrixTimeoutUnbounded,
            OperatorRefusal::TimeoutExceedsTierCap {
                declared_ms: 2,
                cap_ms: 1,
            },
            OperatorRefusal::MatrixTimeoutLeavesNoRoomForCleanup {
                per_matrix_ms: 1,
                whole_run_ms: 1,
            },
            OperatorRefusal::TooFewEnginesForDifferential {
                declared: 1,
                required: 2,
            },
            OperatorRefusal::TooManyEnginesForAttribution {
                declared: 3,
                cap: 2,
            },
            OperatorRefusal::RetainedMappingReceiptAbsent,
            OperatorRefusal::MappingReceiptUnnamed,
            OperatorRefusal::MappingReadFromWire,
            OperatorRefusal::PidOfferedAsIdentity {
                coordinate: OwnerCoordinate::Instance,
            },
            OperatorRefusal::EndpointOfferedAsIdentity {
                coordinate: OwnerCoordinate::Slot,
            },
            OperatorRefusal::VersionOfferedAsIdentity {
                coordinate: OwnerCoordinate::Environment,
            },
            OperatorRefusal::CoordinateSourceMismatch {
                coordinate: OwnerCoordinate::Slot,
                offered: RetainedSource::EnvironmentSidecarDigest,
            },
            OperatorRefusal::RetainedInstanceIdentityAbsent,
            OperatorRefusal::RetainedSlotAmbiguous,
            OperatorRefusal::BodyContentComparisonUnproven,
            OperatorRefusal::ObservationDigestAbsent {
                operation_id: "v2.health.get".to_owned(),
            },
        ]
    }

    #[test]
    fn every_own_variant_maps_to_its_own_label_and_every_index_is_covered_once() {
        let mut covered = [false; OperatorRefusal::OWN_LABEL_COUNT];
        for refusal in one_of_every_own_refusal() {
            let index = match refusal.own_label_index() {
                Some(index) => index,
                None => panic!(
                    "{} is one of this lane's own refusals and must carry an index",
                    refusal.label()
                ),
            };
            let Some(slot) = covered.get_mut(index) else {
                panic!(
                    "{} indexes past the end of OWN_LABELS, so the array and the match disagree",
                    refusal.label()
                );
            };
            assert!(
                !*slot,
                "index {index} is claimed by two variants, so one law would take another's corpus row"
            );
            *slot = true;
            assert_eq!(
                OperatorRefusal::OWN_LABELS.get(index).copied(),
                Some(refusal.label()),
                "a variant's index must point at its own label, or the projection files it under another law"
            );
        }
        let uncovered: Vec<usize> = covered
            .iter()
            .enumerate()
            .filter(|(_, seen)| !**seen)
            .map(|(index, _)| index)
            .collect();
        assert!(
            uncovered.is_empty(),
            "OWN_LABELS index/indices {uncovered:?} are produced by no variant in the sample -- a label with no law behind it, or a variant this sample forgot"
        );
    }

    #[test]
    fn this_lanes_labels_are_disjoint_from_every_landed_one() {
        for own in OperatorRefusal::OWN_LABELS {
            assert!(
                !DifferentialRefusal::ALL_LABELS.contains(&own),
                "{own} is already a landed word: this lane mints no second spelling of a landed law"
            );
        }
    }
}

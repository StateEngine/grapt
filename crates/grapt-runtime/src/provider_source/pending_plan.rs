//! Own one prepared request while its read-only context admission waits.
use super::{
    compaction, AttemptPlan, ContextAdmissionRefusal, PlanRefusal, PlanRefusalCause, PlannedAttempt,
};
use crate::attempt_admission::AttemptAdmissionRefusal;
use crate::step::HistoryGeneration;
use grapt_types::StepNumber;
use std::sync::Mutex;

struct HeldPlan {
    step: StepNumber,
    ordinal: u32,
    generation: HistoryGeneration,
    planned: PlannedAttempt,
}

#[derive(Default)]
pub(super) struct PlanState(Option<HeldPlan>);

impl PlanState {
    pub(super) fn pending(
        &self,
        step: StepNumber,
        ordinal: u32,
        generation: HistoryGeneration,
    ) -> bool {
        self.0.as_ref().is_some_and(|held| {
            held.step == step && held.ordinal == ordinal && held.generation == generation
        })
    }

    pub(super) fn clear(&mut self) {
        self.0 = None;
    }

    pub(super) fn prepare<P: AttemptPlan>(
        &mut self,
        planner: &mut P,
        step: StepNumber,
        ordinal: u32,
        generation: HistoryGeneration,
        compaction: &Mutex<compaction::CompactionBinding>,
    ) -> Result<PlannedAttempt, AttemptAdmissionRefusal> {
        let held = self.0.take().filter(|held| {
            held.step == step && held.ordinal == ordinal && held.generation == generation
        });
        let planned = match held {
            Some(held) => held.planned,
            None => planner
                .plan(step, ordinal, generation)
                .map_err(|refusal: PlanRefusal| {
                    refusal.into_refusal(compaction, ordinal, generation)
                })?,
        };
        match planner.context_admission(&planned) {
            Ok(None) => Ok(planned),
            Ok(Some(trigger)) => {
                let mut binding = compaction::lock(compaction);
                binding.generation = Some(generation);
                binding.preceding_ordinal = ordinal.checked_sub(1);
                Err(AttemptAdmissionRefusal::CompactionRequired { trigger })
            }
            Err(ContextAdmissionRefusal::CapacityBusy) => {
                self.0 = Some(HeldPlan {
                    step,
                    ordinal,
                    generation,
                    planned,
                });
                Err(AttemptAdmissionRefusal::CapacityBusy)
            }
            Err(ContextAdmissionRefusal::Context(failure)) => {
                Err(AttemptAdmissionRefusal::ContextUnavailable {
                    failure,
                    continuity: None,
                })
            }
        }
    }
}

impl PlanRefusal {
    pub(super) fn into_refusal(
        self,
        compaction: &std::sync::Mutex<super::compaction::CompactionBinding>,
        ordinal: u32,
        generation: HistoryGeneration,
    ) -> crate::attempt_admission::AttemptAdmissionRefusal {
        use crate::attempt_admission::AttemptAdmissionRefusal;
        if self.cause == PlanRefusalCause::CapacityBusy {
            AttemptAdmissionRefusal::CapacityBusy
        } else if let Some(trigger) = self.compaction {
            let mut binding = super::compaction::lock(compaction);
            binding.generation = Some(generation);
            binding.preceding_ordinal = ordinal.checked_sub(1);
            AttemptAdmissionRefusal::CompactionRequired { trigger }
        } else {
            AttemptAdmissionRefusal::PlanRefused {
                detail: self.detail,
            }
        }
    }
}

//! Pending fold: admitted → pending row; promoted → row cleared in place.
//! Final state = unpromoted work only (the oracle's `session_pending`).

use crate::event::{InboxEvent, OperationDeliverySettlement};
use grapt_types::{
    AdmissionSeq, ArtifactRef, DeliveryClaimId, DeliveryMode, MessageId, OperationId,
    RuntimeGeneration, SessionId, SubagentOperationId,
};
use std::collections::HashMap;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingRow {
    pub session: SessionId,
    pub delivery: DeliveryMode,
    /// The admitted event's durable sequence within its aggregate — this is
    /// what the oracle stores as `session_pending.admitted_seq`.
    pub admitted_seq: AdmissionSeq,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OperationDeliveryRow {
    pub session: SessionId,
    pub operation: OperationId,
    pub subagent: SubagentOperationId,
    pub claim: DeliveryClaimId,
    pub generation: RuntimeGeneration,
    pub settlement: OperationDeliverySettlement,
    pub artifact: Option<ArtifactRef>,
    pub projection: String,
    pub admitted_seq: AdmissionSeq,
}

#[derive(Debug, Default)]
pub struct InboxFold {
    pub pending: HashMap<MessageId, PendingRow>,
    pub operation_deliveries: HashMap<MessageId, OperationDeliveryRow>,
    /// The one pending user-requested compaction per Session. Kept apart from
    /// `pending` and the counters, which describe inputs.
    pub compactions: HashMap<SessionId, MessageId>,
    pub admitted: u64,
    pub promoted: u64,
    pub violations: Vec<String>,
    /// Promotions whose admission is not in the log — legal-looking under
    /// compaction holes, so measured rather than fatal; the real clone
    /// yields zero.
    pub unmatched_promotions: Vec<String>,
}

impl InboxFold {
    /// `event_seq` is the durable event's own sequence (zero-based law).
    pub fn apply(&mut self, event: InboxEvent, event_seq: u64) {
        match event {
            InboxEvent::Admitted {
                session,
                input,
                delivery,
            } => {
                self.admitted += 1;
                if self
                    .pending
                    .insert(
                        input.clone(),
                        PendingRow {
                            session,
                            delivery,
                            admitted_seq: AdmissionSeq::new(event_seq),
                        },
                    )
                    .is_some()
                {
                    self.violations
                        .push(format!("{input}: admitted twice while pending"));
                }
            }
            InboxEvent::SyntheticAdmitted {
                session,
                input,
                delivery,
            } => {
                self.admitted += 1;
                if self
                    .pending
                    .insert(
                        input.clone(),
                        PendingRow {
                            session,
                            delivery,
                            admitted_seq: AdmissionSeq::new(event_seq),
                        },
                    )
                    .is_some()
                {
                    self.violations
                        .push(format!("{input}: admitted twice while pending"));
                }
            }
            InboxEvent::Promoted { input, .. } => {
                self.promoted += 1;
                if self.pending.remove(&input).is_none() {
                    self.unmatched_promotions.push(input.to_string());
                }
            }
            InboxEvent::OperationDeliveryAdmitted {
                session,
                input,
                operation,
                subagent,
                claim,
                generation,
                settlement,
                artifact,
                projection,
            } => {
                self.admitted += 1;
                if self
                    .operation_deliveries
                    .insert(
                        input.clone(),
                        OperationDeliveryRow {
                            session,
                            operation,
                            subagent,
                            claim,
                            generation,
                            settlement,
                            artifact,
                            projection,
                            admitted_seq: AdmissionSeq::new(event_seq),
                        },
                    )
                    .is_some()
                {
                    self.violations
                        .push(format!("{input}: operation delivery admitted twice"));
                }
            }
            InboxEvent::LegacySyntheticAdmitted { .. } | InboxEvent::AgentMessageAdmitted(_) => {}
            InboxEvent::CompactionAdmitted { session, input } => {
                if let Some(pending) = self.compactions.insert(session.clone(), input.clone()) {
                    self.violations.push(format!(
                        "{input}: compaction admitted while {pending} was pending in {session}"
                    ));
                }
            }
            InboxEvent::CompactionSettled { session } => {
                self.compactions.remove(&session);
            }
        }
    }
}

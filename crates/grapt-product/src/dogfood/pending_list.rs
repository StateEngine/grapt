//! `v2.session.pending.list`: admitted work not yet in projected history, in
//! admission order, including a pending compaction barrier.
use super::*;

impl DogfoodApplication {
    pub(super) fn pending_list(
        &self,
        session_id: WireId<SessionId>,
    ) -> Result<grapt_api::u3a::SessionPendingListResponseDto, SessionLookupApplicationError> {
        let session = session_id.into_inner();
        context::require_session(self, &session)?;
        let held = self.lock_inbox();
        let pending = held
            .pending_inputs(&session)
            .map_err(|error| SessionLookupApplicationError::InvalidRequest(invalid(error)))?;
        // The oracle lists a pending compaction barrier among the inputs,
        // in admission order (`v2.session.pending.list`).
        let mut barrier = held
            .pending_compaction(&session)
            .map_err(|error| SessionLookupApplicationError::InvalidRequest(invalid(error)))?;
        let compaction_row = |barrier: grapt_inbox::PendingCompaction| {
            decode(json!({
                "type": "compaction", "admittedSeq": barrier.seq,
                "id": barrier.input.as_str(), "sessionID": session.as_str(),
                "timeCreated": barrier.created_ms,
            }))
            .map(Box::new)
            .map(grapt_api::u3a::SessionPendingInfoDto::Compaction)
            .map_err(|error| SessionLookupApplicationError::InvalidRequest(invalid(error)))
        };
        let mut rows = Vec::with_capacity(pending.len() + 1);
        for row in pending {
            if let Some(earlier) = barrier.take_if(|barrier| barrier.seq < row.seq) {
                rows.push(compaction_row(earlier)?);
            }
            let mut data = json!({"text":row.text});
            if let Some(description) = row.description {
                data["description"] = json!(description);
            }
            if let Some(metadata) = row.metadata {
                data["metadata"] = match row.kind {
                    grapt_inbox::PromotedInputKind::User => metadata,
                    grapt_inbox::PromotedInputKind::Synthetic => {
                        super::input_metadata::public_metadata(metadata)
                    }
                };
            }
            let value = json!({
                "type":match row.kind {
                    grapt_inbox::PromotedInputKind::User => "user",
                    grapt_inbox::PromotedInputKind::Synthetic => "synthetic",
                },
                "admittedSeq":row.seq, "id":row.input.as_str(), "sessionID":session.as_str(),
                "timeCreated":row.created_ms,
                "data":data,
                "delivery":match row.delivery { DeliveryMode::Steer => "steer", DeliveryMode::Queue => "queue" },
            });
            let decoded = match row.kind {
                grapt_inbox::PromotedInputKind::User => decode(value)
                    .map(Box::new)
                    .map(grapt_api::u3a::SessionPendingInfoDto::User),
                grapt_inbox::PromotedInputKind::Synthetic => decode(value)
                    .map(Box::new)
                    .map(grapt_api::u3a::SessionPendingInfoDto::Synthetic),
            }
            .map_err(|error| SessionLookupApplicationError::InvalidRequest(invalid(error)))?;
            rows.push(decoded);
        }
        if let Some(last) = barrier {
            rows.push(compaction_row(last)?);
        }
        let rows = grapt_api::u3a::BoundedVec::try_new(rows)
            .map_err(|error| SessionLookupApplicationError::InvalidRequest(invalid(error)))?;
        Ok(grapt_api::u3a::SessionPendingListResponseDto::new(
            grapt_schema::Required::new(rows),
        ))
    }
}

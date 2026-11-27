//! `session.compact` (CODING-QUALITY-01 /compact slice 3): admit a durable
//! compaction request. It runs in the Session's next input's first Step
//! (slice 2), so admission neither wakes nor runs anything.
use super::*;
use grapt_api::u3a::{CompactRequestDto, CompactResponseDto, SessionCompactApplicationError};

impl DogfoodApplication {
    pub(crate) fn submit_compact(
        &self,
        session_id: WireId<SessionId>,
        request: CompactRequestDto,
    ) -> Result<CompactResponseDto, SessionCompactApplicationError> {
        let session = session_id.into_inner();
        // Only an unknown Session is 404. A corrupt seed or a failed store
        // read is an explicit refusal, as on every Session route, and no
        // barrier is admitted (review of 71949676).
        context::session_record(self, &session).map_err(|error| match error {
            SessionLookupApplicationError::Unavailable => {
                SessionCompactApplicationError::InvalidRequest(invalid(
                    "History is temporarily unavailable; retry the request",
                ))
            }
            SessionLookupApplicationError::SessionNotFound(value) => {
                SessionCompactApplicationError::SessionNotFound(value)
            }
            SessionLookupApplicationError::InvalidRequest(value) => {
                SessionCompactApplicationError::InvalidRequest(value)
            }
        })?;
        let instructions = request
            .instructions()
            .as_present()
            .map(|text| text.as_str());
        if instructions.is_some_and(|text| text.trim().is_empty() || text.len() > 16 * 1024) {
            return Err(SessionCompactApplicationError::InvalidRequest(invalid(
                "compaction instructions must contain text and fit within 16 KiB",
            )));
        }
        let input: MessageId = match request.id().as_present() {
            Some(id) => id.as_inner().clone(),
            None => format!("msg_{}", self.ids.mint().as_str())
                .parse()
                .map_err(|_| {
                    SessionCompactApplicationError::InvalidRequest(invalid(
                        "minted compaction id is invalid",
                    ))
                })?,
        };
        let event: EventId = format!("evt_{}", self.ids.mint().as_str())
            .parse()
            .map_err(|_| {
                SessionCompactApplicationError::InvalidRequest(invalid(
                    "compaction event id is invalid",
                ))
            })?;
        self.ensure_session_writer(&session)
            .map_err(|error| SessionCompactApplicationError::Conflict(conflict(error)))?;
        let admitted = self
            .lock_inbox()
            .admit_compaction(grapt_inbox::CompactionAdmitRequest {
                event_id: &event,
                session: &session,
                input: &input,
                created_ms: self.clock.now().as_millis(),
                instructions,
            })
            // An id already used by another input, a compaction that already
            // ran, or a store refusal: none is a request the caller can retry
            // unchanged, which is the frozen route's 409.
            .map_err(|error| SessionCompactApplicationError::Conflict(conflict(error)))?;
        let pending = admitted.pending();
        let value = json!({"data": {
            "admittedSeq": pending.seq,
            "id": pending.input.as_str(),
            "sessionID": session.as_str(),
            "timeCreated": pending.created_ms,
            "type": "compaction",
        }});
        decode(value)
            .map_err(|error| SessionCompactApplicationError::InvalidRequest(invalid(error)))
    }
}

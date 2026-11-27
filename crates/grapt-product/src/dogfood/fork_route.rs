//! Fork requests use the existing bounded Session copy and commit owner.
use super::*;
use grapt_api::u3a::SessionForkRequestDto;

impl DogfoodApplication {
    pub(super) fn fork_session_request(
        &self,
        parent: SessionId,
        request: SessionForkRequestDto,
    ) -> Result<SessionResponseDto, SessionLookupApplicationError> {
        context::require_session(self, &parent)?;
        let value = serde_json::to_value(&request)
            .map_err(|error| SessionLookupApplicationError::InvalidRequest(invalid(error)))?;
        let authorities = DeterministicAuthorities::new(&*self.clock, &*self.ids);
        let mut inbox_projector = grapt_inbox::InboxProjector;
        let mut inbox = self.lock_inbox();
        let forked = match (
            value.get("throughSeq").and_then(Value::as_u64),
            request.before_message_id().as_present(),
        ) {
            (Some(_), Some(_)) => {
                return Err(SessionLookupApplicationError::InvalidRequest(invalid(
                    "Use throughSeq or beforeMessageID, not both.",
                )))
            }
            (None, Some(before)) => grapt_session::fork_session_before_prompt(
                inbox.store_mut(),
                &authorities,
                &parent,
                before.as_inner(),
                &mut [&mut inbox_projector],
            ),
            (Some(seq), None) => grapt_session::fork_session_at_seq(
                inbox.store_mut(),
                &authorities,
                &parent,
                grapt_types::EventSeq::new(seq),
                &mut [&mut inbox_projector],
            ),
            (None, None) => grapt_session::fork_session(
                inbox.store_mut(),
                &authorities,
                &parent,
                &mut [&mut inbox_projector],
            ),
        }
        .map_err(|error| SessionLookupApplicationError::InvalidRequest(invalid(error)))?;
        drop(inbox);
        let record = self
            .get_record(&forked.session)
            .map_err(|error| SessionLookupApplicationError::InvalidRequest(invalid(error)))?;
        self.session_response(&record)
            .map_err(SessionLookupApplicationError::InvalidRequest)
    }
}

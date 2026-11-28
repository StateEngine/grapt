//! Conversation editing retains the event ledger and performs no file effects.

use super::*;
use grapt_api::u3a::{
    SessionRewindApplicationError as Error, SessionRewindPreviewRequestDto,
    SessionRewindPreviewResponseDto, SessionRewindRequestDto, SessionRewindResponseDto,
};
use grapt_session::rewind::{self, RewindError};

fn lookup(application: &ServerApplication, session: &SessionId) -> Result<(), Error> {
    context::require_session(application, session).map_err(|error| match error {
        SessionLookupApplicationError::Unavailable => Error::InvalidRequest(invalid(
            "History is temporarily unavailable; retry the request",
        )),
        SessionLookupApplicationError::SessionNotFound(value) => Error::SessionNotFound(value),
        SessionLookupApplicationError::InvalidRequest(value) => Error::InvalidRequest(value),
    })
}

fn refusal(error: RewindError) -> Error {
    let text = error.to_string();
    match error {
        RewindError::Stale | RewindError::Pending => Error::Conflict(conflict(text)),
        RewindError::NotUserPrompt
        | RewindError::AddressTooLong
        | RewindError::PromptTooLong
        | RewindError::SelectionBound
        | RewindError::MidExecution => Error::InvalidRequest(invalid(text)),
        _ => Error::Unknown(unknown(text)),
    }
}

impl ServerApplication {
    pub(crate) fn preview_rewind(
        &self,
        session_id: WireId<SessionId>,
        request: SessionRewindPreviewRequestDto,
    ) -> Result<SessionRewindPreviewResponseDto, Error> {
        let session = session_id.into_inner();
        lookup(self, &session)?;
        let value =
            serde_json::to_value(request).map_err(|error| Error::InvalidRequest(invalid(error)))?;
        let prompts = value
            .get("prompts")
            .and_then(Value::as_u64)
            .filter(|count| (1..=200).contains(count))
            .ok_or_else(|| {
                Error::InvalidRequest(invalid("Use /rewind or /rewind <1–200 earlier prompts>."))
            })?;
        let inbox = self.lock_inbox();
        let selected =
            rewind::previous(inbox.store(), &session, prompts as usize).map_err(refusal)?;
        if selected.prompt().len() > grapt_api::u3a::limits::MAX_PROMPT_TEXT_BYTES {
            return Err(Error::InvalidRequest(invalid(
                "The selected prompt exceeds the native composer limit.",
            )));
        }
        decode(json!({"data":{
            "sessionID":session.as_str(), "messageID":selected.before().as_str(),
            "tip":selected.tip().value(), "prompt":selected.prompt(),
        }}))
        .map_err(|error| Error::Unknown(unknown(error)))
    }

    pub(crate) fn commit_rewind(
        &self,
        session_id: WireId<SessionId>,
        request: SessionRewindRequestDto,
    ) -> Result<SessionRewindResponseDto, Error> {
        let session = session_id.into_inner();
        lookup(self, &session)?;
        // The same slot excludes admitted and active owners in both backends.
        // It is acquired before the inbox, and no guard crosses an await.
        let _idle = self
            .wake_slots
            .reserve_idle(&session)
            .map_err(|error| Error::Conflict(conflict(error)))?;
        self.ensure_session_writer(&session)
            .map_err(|error| Error::Conflict(conflict(error)))?;
        let value =
            serde_json::to_value(request).map_err(|error| Error::InvalidRequest(invalid(error)))?;
        let before: MessageId = value
            .get("messageID")
            .and_then(Value::as_str)
            .ok_or_else(|| Error::InvalidRequest(invalid("rewind needs a selected prompt")))?
            .parse()
            .map_err(|_| Error::InvalidRequest(invalid("rewind prompt identity is invalid")))?;
        let tip = value
            .get("tip")
            .and_then(Value::as_u64)
            .ok_or_else(|| Error::InvalidRequest(invalid("rewind needs its preview tip")))?;
        let mut inbox = self.lock_inbox();
        continuation::require_resolved_effects(inbox.store(), &session)
            .map_err(|error| Error::Conflict(conflict(error)))?;
        let selection = rewind::for_commit(
            inbox.store(),
            &session,
            &before,
            grapt_types::EventSeq::new(tip),
        )
        .map_err(refusal)?;
        // Validate the response vocabulary before the irreversible append.
        let _: SessionRewindPreviewResponseDto = decode(json!({"data": {
            "sessionID":session.as_str(), "messageID":before.as_str(),
            "tip":tip, "prompt":selection.prompt(),
        }}))
        .map_err(|error| Error::InvalidRequest(invalid(error)))?;
        let authorities = DeterministicAuthorities::new(&*self.clock, &*self.ids);
        let committed = rewind::commit(
            inbox.store_mut(),
            &authorities,
            &selection,
            &mut [&mut grapt_instruction::InstructionProjector],
        )
        .map_err(refusal)?;
        // Every subscriber must re-read rather than retaining messages removed
        // from the conversation. This advisory failure cannot undo the commit.
        self.hub
            .invalidate_session_because(&session, "conversation rewound; reload durable history");
        decode(json!({"data":{
            "sessionID":session.as_str(), "messageID":before.as_str(),
            "committedSeq":committed, "prompt":selection.prompt(),
        }}))
        .map_err(|error| Error::Unknown(unknown(error)))
    }
}

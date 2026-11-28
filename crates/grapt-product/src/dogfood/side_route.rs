//! A side question uses an uncommitted conversation snapshot and ordinary provider I/O.
use super::*;
use grapt_api::u3a::{SideQuestionRequestDto, SideQuestionResponseDto};

impl ServerApplication {
    pub(crate) fn answer_side_question(
        &self,
        session: SessionId,
        request: SideQuestionRequestDto,
    ) -> ApplicationFuture<SideQuestionResponseDto, SessionLookupApplicationError> {
        let allowed = context::require_session(self, &session);
        let executor = Arc::clone(&self.executor);
        Box::pin(async move {
            allowed?;
            let prompt = request.prompt().get().as_str();
            if prompt.trim().is_empty() || prompt.len() > 65_536 {
                return Err(SessionLookupApplicationError::InvalidRequest(invalid(
                    "A side question must contain 1–65536 bytes of text.",
                )));
            }
            let executor = &executor.0;
            let text = executor
                .transient_generate(
                    session,
                    prompt.to_owned(),
                    execution::TransientContext::SideQuestion,
                )
                .await
                .map_err(|error| SessionLookupApplicationError::InvalidRequest(invalid(error)))?;
            Ok(static_dto::<SideQuestionResponseDto>(json!({"text":text})))
        })
    }
}

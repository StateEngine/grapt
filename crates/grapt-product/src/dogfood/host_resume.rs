//! Explicit host resume changes native eligibility, never admits replacement input.
use super::host_control::refusal;
use super::*;
use grapt_api::u3a::{HostNoticeBindingDto, NativeMailResumeRequestDto};

impl ServerApplication {
    pub(super) fn resume_host_native_mail(
        &self,
        session: &SessionId,
        request: NativeMailResumeRequestDto,
    ) -> Result<HostNoticeBindingDto, SessionPromptApplicationError> {
        self.get_record(session)
            .map_err(|_| SessionPromptApplicationError::SessionNotFound(not_found(session)))?;
        let service = self.host_service_identity()?;
        if serde_json::to_value(service).map_err(refusal)?
            != serde_json::to_value(request.service().get()).map_err(refusal)?
        {
            return Err(refusal("resume belongs to another service incarnation"));
        }
        self.ensure_session_writer(session).map_err(refusal)?;
        let event = self.mint_event().map_err(refusal)?;
        {
            // Match capability replacement's Inbox-then-custody lock order.
            // Both guards cover the durable release; neither crosses async I/O.
            let mut inbox = self.lock_inbox();
            let custody = self
                .session_capabilities
                .lock()
                .map_err(|_| refusal("Session capability custody is unavailable"))?;
            if !custody.values.contains_key(session)
                || !custody.native_principals.contains_key(session)
            {
                return Err(refusal(
                    "Session requires an authenticated capability rebind",
                ));
            }
            inbox
                .resume_native_mail_target(grapt_inbox::NativeMailResume {
                    session,
                    hold: request.hold_id().get().as_inner(),
                    target: request.target_id().get().as_inner(),
                    binding: request.binding().get().as_str(),
                    event: &event,
                    created_ms: self.clock.now().as_millis(),
                })
                .map_err(refusal)?;
        }
        // Release is not an execution claim. Existing keyed notice reconciliation
        // is the only path that requests a new external wake owner.
        self.host_notice_binding(session)
    }
}

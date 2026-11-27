//! Live backoff projection. The Step driver owns all policy and waiting.
use super::*;
use grapt_runtime::step::RetryNotice;

impl SessionStatusEmitter {
    pub(in crate::dogfood) fn provider_slot_info(
        &self,
        provider: &grapt_types::ProviderId,
        position: usize,
    ) -> Option<SessionStatusInfo> {
        let now = u64::try_from(self.clock.now().as_millis()).ok()?;
        Some(SessionStatusInfo::retry(
            0.into(),
            format!("Waiting for provider slot (position {position})"),
            Some(
                grapt_api::u3a::SessionStatusAction::new(
                    "provider_slot".into(),
                    provider.as_str().into(),
                    "Waiting for provider slot".into(),
                    "Work resumes when a slot is free; Esc cancels.".into(),
                    "Wait".into(),
                    None,
                )
                .with_queue_position(u64::try_from(position).ok()?.into()),
            ),
            now.into(),
        ))
    }

    pub(in crate::dogfood) fn waiting_info(
        &self,
        provider: &grapt_types::ProviderId,
        notice: RetryNotice,
    ) -> Option<SessionStatusInfo> {
        use grapt_types::ProviderFailureClass;
        let (reason, message) = match (notice.preparation_stage, notice.class) {
            (Some(_), _) => (
                "context_preparation",
                "Context preparation stalled; retrying automatically.",
            ),
            (None, Some(ProviderFailureClass::RateLimit)) => {
                ("rate_limited", "Provider busy, retrying.")
            }
            (None, Some(ProviderFailureClass::ProviderInternal)) => (
                "provider_unavailable",
                "Provider temporarily unavailable; retrying automatically.",
            ),
            (None, Some(ProviderFailureClass::Authentication)) => (
                "credential_refreshed",
                "Provider credentials refreshed; retrying automatically.",
            ),
            (None, Some(ProviderFailureClass::Transport)) => (
                "transport",
                "Provider connection interrupted; retrying automatically.",
            ),
            _ => ("retrying", "Provider request will retry automatically."),
        };
        let (reason, message) = match notice.capacity_wait {
            Some(elapsed) => (
                "local_capacity",
                format!("Waiting for capacity (waited {}s).", elapsed.as_secs()),
            ),
            None => (reason, message.to_owned()),
        };
        let delay = u64::try_from(notice.after.as_millis()).ok()?;
        let now = u64::try_from(self.clock.now().as_millis()).ok()?;
        let next = now.checked_add(delay)?;
        Some(SessionStatusInfo::retry(
            u64::from(notice.attempt).into(),
            message,
            Some(grapt_api::u3a::SessionStatusAction::new(
                reason.into(),
                provider.as_str().into(),
                if notice.capacity_wait.is_some() {
                    "Waiting for capacity"
                } else {
                    "Automatic retry"
                }
                .into(),
                "Wait automatically; Esc interrupts.".into(),
                "Wait".into(),
                None,
            )),
            next.into(),
        ))
    }

    pub(super) fn build_info(
        &self,
        session: &SessionId,
        status: SessionStatusInfo,
    ) -> Option<DogfoodEvent> {
        self.build_info_phase(session, status, None)
    }

    pub(in crate::dogfood) fn build_info_phase(
        &self,
        session: &SessionId,
        status: SessionStatusInfo,
        phase: Option<grapt_api::u3a::SessionStatusPhase>,
    ) -> Option<DogfoodEvent> {
        let created = FiniteF64::try_new(self.clock.now().as_millis() as f64).ok()?;
        let event = format!("evt_{}", self.ids.mint().as_str())
            .parse::<EventId>()
            .ok()?;
        Some(DogfoodEvent::SessionStatus(LiveEvent::new(
            WireId::<EventId>::parse(event.as_str().to_owned())?,
            created,
            SessionStatusData::new(WireId::parse(session.as_str().to_owned())?, status)
                .with_phase(phase),
        )))
    }

    pub(in crate::dogfood) fn note_active(&self, session: &SessionId) {
        self.lock_owed().insert(session.clone());
        self.record(session, StatusKind::Busy);
    }

    pub(in crate::dogfood) fn publish_status_if_phase(
        &self,
        session: &SessionId,
        status: SessionStatusInfo,
        phase: Option<grapt_api::u3a::SessionStatusPhase>,
        current: impl FnOnce() -> bool,
    ) {
        let Some(event) = self.build_info_phase(session, status, phase) else {
            return;
        };
        if let Err(error) = self.hub.publish_if(&event, current) {
            self.hub.invalidate_because(&format!(
                "session {session}: retry status publication unavailable: {error}"
            ));
        }
    }

    pub(in crate::dogfood) fn offer_active_phase(
        &self,
        session: &SessionId,
        status: SessionStatusInfo,
        phase: Option<grapt_api::u3a::SessionStatusPhase>,
        frames: &mut Vec<DogfoodEvent>,
    ) -> bool {
        if frames.len() >= HYDRATION_CAP {
            return false;
        }
        if let Some(event) = self.build_info_phase(session, status, phase) {
            self.note_active(session);
            frames.push(event);
        }
        true
    }
}

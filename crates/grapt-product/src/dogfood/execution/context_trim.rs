//! Publish a successful degraded request through the existing notice owner.
use super::*;
use grapt_session::notice::{ContextEstimate, ContextTrimNotice};

pub(in crate::dogfood) const LABEL: &str = "Context trimmed: older complete exchanges were omitted from this request because automatic compaction could not reduce the context enough. Current instructions, authorization and the latest user prompt are retained; your durable history is intact.";

pub(in crate::dogfood) struct TrimFeedback {
    inbox: Arc<Mutex<Inbox>>,
    ids: Arc<OsRandomIds>,
    clock: Arc<SystemClock>,
    hub: Arc<DogfoodEventHub>,
    session: SessionId,
    execution: ExecutionId,
}
impl TrimFeedback {
    pub(super) fn new(location: &LiveLocation, session: SessionId, execution: ExecutionId) -> Self {
        Self {
            inbox: location.inbox.clone(),
            ids: location.ids.clone(),
            clock: location.clock.clone(),
            hub: location.hub.clone(),
            session,
            execution,
        }
    }
    pub(in crate::dogfood) fn record(
        &self,
        estimate: ContextEstimate,
        window_floor: Option<grapt_types::EventSeq>,
    ) -> Result<(), String> {
        let mut inbox = match self.inbox.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        let authorities = DeterministicAuthorities::new(&*self.clock, &*self.ids);
        grapt_session::notice::record_context_trimmed(
            inbox.store_mut(),
            &authorities,
            &self.session,
            &self.execution,
            estimate,
            window_floor,
        )?;
        drop(inbox);
        crate::server_log::info(format_args!(
            "session {}: {LABEL}; local estimate {} -> {}, reserve {}, budget {}",
            self.session, estimate.before, estimate.after, estimate.output_reserve, estimate.budget
        ));
        self.hub.reconcile_because(
            grapt_api::ReconcileReason::UnpublishedAdvisoryNotice,
            "context trimmed notice committed",
        );
        Ok(())
    }
}
pub(in crate::dogfood) fn row(notice: ContextTrimNotice) -> Result<(u64, String, String), String> {
    let suffix = notice
        .position
        .event
        .as_str()
        .strip_prefix("evt_")
        .ok_or("context trim event has no prefix")?;
    let id = format!("msg_{suffix}");
    let message: grapt_api::u3a::SessionMessageSystemDto = super::super::decode(
        serde_json::json!({
            "type":"system", "id":id, "time":{"created":notice.position.created_ms}, "text":LABEL,
            "metadata":{"contextTrimmed":true,"executionID":notice.execution.as_str(),"contextEstimate":notice.estimate.metadata(),
                "omittedBeforeSeq":notice.window_floor.map(grapt_types::EventSeq::value)}
        }),
    )?;
    let raw = serde_json::to_string(&message).map_err(|error| error.to_string())?;
    Ok((notice.position.seq, id, raw))
}

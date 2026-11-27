//! Product projection of an advisory provider diagnosis for one committed Step.
use super::*;

pub(super) fn record(sink: &LiveSink, failure: &grapt_runtime::owner::StepFailure) {
    let Some(diagnostic) = failure.diagnostic() else {
        return;
    };
    let result = {
        let mut inbox = match sink.inbox.lock() {
            Ok(held) => held,
            Err(poisoned) => poisoned.into_inner(),
        };
        let authorities = DeterministicAuthorities::new(&*sink.clock, &*sink.ids);
        grapt_session::notice::record_step_failure(
            inbox.store_mut(),
            &authorities,
            &sink.session,
            &sink.execution,
            sink.step,
            diagnostic,
        )
    };
    if let Err(error) = result {
        crate::diagnostics::report(format!(
            "session {} step {}: failure explanation unavailable: {error}",
            sink.session,
            sink.step.value()
        ));
    }
}
pub(crate) fn row(
    notice: grapt_session::notice::StepFailureNotice,
) -> Result<(u64, String, String), String> {
    let suffix = notice
        .position
        .event
        .as_str()
        .strip_prefix("evt_")
        .ok_or("notice event identity has no prefix")?;
    let id = format!("msg_{suffix}");
    let message: grapt_api::u3a::SessionMessageSystemDto = super::super::decode(
        serde_json::json!({"type":"system","id":id,
        "metadata":{"stepFailureDetail":true,"executionID":notice.execution.as_str(),"step":notice.step.value()},
        "time":{"created":notice.position.created_ms},"text":notice.text()}),
    )?;
    let raw = serde_json::to_string(&message).map_err(|error| error.to_string())?;
    Ok((notice.position.seq, id, raw))
}

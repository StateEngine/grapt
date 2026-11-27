//! Reconcile through the existing subagent and inbox owners at safe boundaries.
use super::*;
use grapt_inbox::operation_mail::OperationMailBatch;
use grapt_subagent::{DeliveryLimits, SubagentService};

pub(super) fn reconcile(inbox: &Arc<Mutex<Inbox>>, parent: &SessionId) -> Result<bool, String> {
    let mut held = match inbox.lock() {
        Ok(held) => held,
        Err(poisoned) => poisoned.into_inner(),
    };
    let store = held.store_mut();
    SubagentService::acknowledge_presented_on(store, parent).map_err(|error| error.to_string())?;
    if OperationMailBatch::has_unpresented_results(store, parent)? {
        return Ok(true);
    }
    let limits = DeliveryLimits::new(64, 1024 * 1024).ok_or("invalid agent mailbox bounds")?;
    SubagentService::publish_background_on(store, parent, limits)
        .map_err(|error| error.to_string())?;
    OperationMailBatch::has_unpresented(store, parent)
}

pub(super) fn prepare(
    inbox: &Arc<Mutex<Inbox>>,
    parent: &SessionId,
    step: grapt_types::StepNumber,
) -> Result<Option<OperationMailBatch>, String> {
    reconcile(inbox, parent)?;
    let held = match inbox.lock() {
        Ok(held) => held,
        Err(poisoned) => poisoned.into_inner(),
    };
    let cause = match SubagentService::child_context_on(held.store(), parent)
        .map_err(|error| error.to_string())?
    {
        Some(context) if matches!(context.status, grapt_subagent::OperationStatus::Running(_)) => {
            SubagentService::followup_cause_on(held.store(), &context.operation)
                .map_err(|error| error.to_string())?
        }
        _ => None,
    };
    OperationMailBatch::prepare_with_priority(held.store(), parent, step, cause.as_ref())
}

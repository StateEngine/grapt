use super::*;

pub(super) fn merge(
    snapshot: &mut AccountingSnapshot,
    ledger: grapt_store::provider_attempt_accounting::ProviderAttemptAccounting,
    history: &MessageHistory,
) -> Result<(), String> {
    let mut indices: HashMap<_, _> = snapshot
        .attempts
        .iter()
        .enumerate()
        .map(|(index, attempt)| (attempt.attempt.clone(), index))
        .collect();
    for record in ledger.attempts {
        let receipt = record.receipt();
        let id = receipt.attempt();
        let index = if let Some(index) = indices.get(id) {
            *index
        } else {
            if snapshot.attempts.len() == MAX_ACCOUNTING_ATTEMPTS {
                return Err("accounting dispatch union exceeds attempt bound".into());
            }
            let index = snapshot.attempts.len();
            snapshot.attempts.push(AccountingAttempt {
                attempt: id.clone(),
                request_cache: None,
                terminal_observed: false,
                operation: AccountingOperation::Unknown,
                send_permitted: false,
                step: None,
                selection: None,
                usage: grapt_types::usage::reconcile_recorded_usage(&[])
                    .map_err(|e| e.to_string())?,
                opening: None,
                first_block: None,
                step_terminal: None,
                completed_at: None,
            });
            indices.insert(id.clone(), index);
            index
        };
        let attempt = &mut snapshot.attempts[index];
        if attempt.step.is_some_and(|step| step != receipt.step()) {
            return Err("accounting dispatch disagrees with event Step".into());
        }
        if let Some(terminal) = history.terminal_positions.get(&receipt.step().value()) {
            if terminal.event != *record.terminal().event()
                || terminal.created_ms != record.terminal().created_ms()
            {
                return Err("accounting dispatch disagrees with terminal coordinates".into());
            }
        }
        attempt.step = Some(receipt.step());
        attempt.send_permitted = true;
        attempt.step_terminal = history.terminals.get(&receipt.step().value()).cloned();
        attempt.completed_at = history
            .completion_observations
            .get(&receipt.step().value())
            .copied();
    }
    snapshot.operational_coverage_complete = ledger.coverage_complete
        && snapshot
            .attempts
            .iter()
            .all(|attempt| attempt.send_permitted);
    Ok(())
}

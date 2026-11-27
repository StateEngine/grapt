use super::*;

fn refusal(record: &AttemptReceiptRecord, detail: &str) -> WriteError {
    WriteError::ReceiptTerminalUnproven {
        attempt: record.receipt.attempt().as_str().to_owned(),
        terminal: record.terminal.event.as_str().to_owned(),
        detail: detail.to_owned(),
    }
}

// Multiple Physical Attempts (including a summary) share their owning Step's
// terminal. Once proven, that fact is authority; per-attempt evidence must not
// be appended again under its identity.
pub(super) fn reuse(
    tx: &GovernedTransaction<'_>,
    record: &AttemptReceiptRecord,
) -> Result<bool, WriteError> {
    let exists: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM event WHERE id = ?1)",
        [record.terminal.event.as_str()],
        |row| row.get(0),
    )?;
    if !exists {
        return Ok(false);
    }
    prove_terminal_within(tx, record)?;
    let (kind, data): (String, String) = tx.query_row(
        "SELECT type, data FROM event WHERE id = ?1",
        [record.terminal.event.as_str()],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    if failure_reason_denies_send_permit(&kind, &data, record)? {
        return Err(refusal(
            record,
            "the terminal denies a surviving attempt's send permit",
        ));
    }
    Ok(true)
}

/// Whether a proven terminal is a known `none-initiated` process loss while
/// this receipt holds a surviving send permit — the contradiction that makes
/// the terminal's proof of "nothing was sent" false.
///
/// Checked for EVERY durable failure terminal version, each read through the
/// codec its name owns (v2 by the failure codec, v3 through the completion
/// observed wrapper). Gating this check on one terminal name let the other
/// version settle a permitted receipt and erase the contradiction (TR-H2).
/// An unknown word decodes to no known reason and never denies: it carries
/// no effect authority.
fn failure_reason_denies_send_permit(
    kind: &str,
    data: &str,
    record: &AttemptReceiptRecord,
) -> Result<bool, WriteError> {
    let reason = match kind {
        STEP_FAILED_TERMINAL => {
            let value = serde_json::from_str(data)
                .map_err(|_| refusal(record, "the proven terminal payload became unreadable"))?;
            let fact = grapt_event::session_step_failed::decode(&value)
                .map_err(|_| refusal(record, "the proven terminal payload became invalid"))?;
            fact.reason().clone()
        }
        STEP_FAILED_TERMINAL_V3 => {
            let value = serde_json::from_str(data)
                .map_err(|_| refusal(record, "the proven terminal payload became unreadable"))?;
            let observed = grapt_event::session_step_observed::decode_failed(&value)
                .map_err(|_| refusal(record, "the proven terminal payload became invalid"))?;
            observed.fact().reason().clone()
        }
        _ => return Ok(false),
    };
    let contradicts = matches!(
        reason,
        grapt_types::StepTerminalReason::ProcessLoss {
            effect: ExternalEffectState::NoneInitiated
        }
    ) && matches!(
        record.receipt.disposition(),
        ReceiptDisposition::Lost(LostEffect::Ambiguous { .. })
    );
    Ok(contradicts)
}

// A reserved-only attempt cannot establish that the entire Step initiated
// nothing. Look for a permitted peer before choosing that classification.
// The existing one-row reader bounds allocation before materialization; this
// additional scan caps total work at 128 rows and 256 KiB, refusing on overflow.
pub(super) fn effect_source(
    tx: &GovernedTransaction<'_>,
    record: &AttemptReceiptRecord,
    ceiling: AttemptRecordCeiling,
) -> Result<AttemptReceiptRecord, WriteError> {
    if matches!(
        record.receipt.disposition(),
        ReceiptDisposition::Lost(LostEffect::Ambiguous { .. })
    ) {
        return Ok(record.clone());
    }
    let mut after = None;
    let mut remaining = 256 * 1024;
    for _ in 0..128 {
        match next_recovery_row(
            tx,
            record.receipt.session(),
            after.as_ref(),
            remaining,
            ceiling,
        )? {
            RecoveryStep::Exhausted => return Ok(record.clone()),
            RecoveryStep::BudgetExhausted { .. } => break,
            RecoveryStep::Row {
                row,
                next_after,
                bytes_charged,
            } => {
                remaining -= bytes_charged;
                after = Some(next_after);
                let RecoveryRow::Receipt(peer) = row else {
                    return Err(refusal(
                        record,
                        "an unreadable peer prevents Step effect classification",
                    ));
                };
                if peer.terminal.event == record.terminal.event {
                    if peer.receipt.step() != record.receipt.step()
                        || peer.terminal.created_ms != record.terminal.created_ms
                        || peer.writer_epoch != record.writer_epoch
                    {
                        return Err(refusal(
                            record,
                            "peer receipts disagree on terminal ownership",
                        ));
                    }
                    if matches!(
                        peer.receipt.disposition(),
                        ReceiptDisposition::Lost(LostEffect::Ambiguous { .. })
                    ) {
                        return Ok(peer);
                    }
                }
            }
        }
    }
    Err(refusal(
        record,
        "the bounded peer scan could not establish the Step's effect",
    ))
}

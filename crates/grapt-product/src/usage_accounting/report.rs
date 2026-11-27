use super::pricing::PricePolicy;
use crate::cache_accounting::CostTruth;
use grapt_session::history::accounting::{AccountingOperation, AccountingSnapshot};
use grapt_types::StepNumber;
use serde::Serialize;
use serde_json::{json, Value};

#[derive(Debug, Serialize)]
pub struct AccountingReport {
    schema: &'static str,
    session: String,
    snapshot_last_seq: Option<u64>,
    enumeration_complete: bool,
    incompleteness_reason: Option<&'static str>,
    coverage: &'static str,
    session_totals: Value,
    observed_subtotals: Value,
    price_policy: Value,
    attempts: Vec<Value>,
}

pub fn report(
    snapshot: AccountingSnapshot,
    policy: Option<(&PricePolicy, &str)>,
) -> Result<AccountingReport, String> {
    let mut attempts = Vec::with_capacity(snapshot.attempts.len());
    for attempt in &snapshot.attempts {
        let usage = &attempt.usage;
        let opened_ms = attempt.opening.as_ref().map(|p| p.created_ms);
        let first_output_ms = attempt.first_block.as_ref().map(|p| p.created_ms);
        let field = |value: Option<&grapt_types::usage::ReconciledUsageField>| -> Value {
            value.map_or_else(|| json!({"status":"unavailable","reason":"not-observed"}), |value| {
                    json!({"status":"observed","tokens":value.count().value(),
                        "provenance":provenance(value.provenance()),"phase":match value.phase() { grapt_types::UsageObservationPhase::DuringStream=>"during-stream", grapt_types::UsageObservationPhase::Terminal=>"terminal", grapt_types::UsageObservationPhase::Reconciled=>"reconciled" }})
                })
        };
        let (cost, reason) = match (policy, &attempt.selection) {
            (Some((policy, _)), Some(selection)) => policy.estimate(
                selection.provider().as_str(),
                selection.model().as_str(),
                usage,
            )?,
            (None, _) => (CostTruth::Unavailable, Some("price-policy-unavailable")),
            (_, None) => (
                CostTruth::Unavailable,
                Some("exact-attempt-model-unavailable"),
            ),
        };
        let cost = match cost {
            CostTruth::Estimated { amount, policy } => {
                json!({"status":"estimated","usd":amount.amount(),"policy":policy.as_str(),"scope":"observed-usage-only",
                    "terminal_usage":attempt.terminal_observed && super::pricing::final_usage(usage)})
            }
            CostTruth::Unavailable => json!({"status":"unavailable","reason":reason}),
            CostTruth::Billed(_) => return Err("derived report cannot claim billed cost".into()),
        };
        let latency = elapsed(opened_ms, first_output_ms)?;
        let completion = attempt.completed_at.map(|at| at.as_millis());
        let completion_latency = elapsed(opened_ms, completion)?;
        attempts.push(json!({"attempt":attempt.attempt.as_str(),"step":attempt.step.map(StepNumber::value),
                "terminal_request_observed":attempt.terminal_observed,
                "request_cache":attempt.request_cache.as_ref().map(request_cache),
                "operation":match attempt.operation { AccountingOperation::Assistant=>"assistant",AccountingOperation::Compaction=>"compaction",AccountingOperation::Unknown=>"unavailable" },
                "dispatch_status":if attempt.send_permitted {"send-permitted"} else {"unavailable"},
                "usage_outcome":super::totals::usage_outcome(attempt),
                "identity_status":if attempt.step.is_some() {"joined"} else {"step-unavailable"},
                "step_terminal_observed":attempt.step_terminal.is_some(),
                "provider":attempt.selection.as_ref().map(|s| s.provider().as_str()),
                "model":attempt.selection.as_ref().map(|s| s.model().as_str()),
                "usage":{"input_total":field(usage.input_total()),"input_fresh":field(usage.input_fresh()),
                    "legacy_input_ambiguous":field(usage.legacy_input()),"output":field(usage.output()),
                    "reasoning":field(usage.reasoning()),"cache_read":field(usage.cache_read()),
                    "cache_write":field(usage.cache_write()),"total":field(usage.total())},
                "cost":cost,"latency":{"basis":"durable-assistant-opening-to-first-block-not-network-first-token",
                    "opened_ms":opened_ms,"first_block_ms":first_output_ms,
                    "opening_event":attempt.opening.as_ref().map(|p|p.event.as_str()),
                    "first_block_event":attempt.first_block.as_ref().map(|p|p.event.as_str()),
                    "first_block_elapsed_ms":latency,"step_completed_ms":completion,
                    "step_completion_elapsed_ms":completion_latency,
                    "missing_endpoint_reason":if latency.is_none() {Some("durable-endpoint-unavailable")} else {None}}}));
    }
    let observed_subtotals = subtotals(&snapshot)?;
    let session_totals = super::totals::session(&snapshot, &observed_subtotals);
    Ok(AccountingReport {
        schema: "secode.session-accounting.v1",
        session: snapshot.session.as_str().to_owned(),
        snapshot_last_seq: snapshot.last_seq,
        enumeration_complete: snapshot.enumeration_complete,
        incompleteness_reason: if snapshot.enumeration_complete {
            None
        } else {
            Some("event-page-bound")
        },
        price_policy: policy.map_or(
            Value::Null,
            |(p, digest)| json!({"version":p.version(),"sha256":digest}),
        ),
        attempts,
        coverage: "send-permit-ledger-and-session-events",
        session_totals,
        observed_subtotals,
    })
}

fn request_cache(request: &grapt_session::history::accounting::AccountingRequest) -> Value {
    // Keep the per-item ladder in durable events; exports need only its comparison boundary.
    let mut record = grapt_event::request_cache::encode(&request.fact);
    if let Some(fp) = record.get_mut("fingerprint").and_then(Value::as_object_mut) {
        fp.remove("prefixes");
    }
    let comparison = request.comparison.as_ref().map(|c| json!({
        "changed_components":c.changed_components,"stable_input_items":c.stable_input_items,
        "stable_input_hash":c.stable_input_hash.as_ref().map(grapt_event::request_cache::hash_hex),
        "complete":c.complete,"append_only":c.append_only,"identical":c.identical,
    }));
    json!({"admitted_ms":request.admitted_ms,
        "previous_attempt":request.previous_attempt.as_ref().map(|a|a.as_str()),
        "record":record,"comparison":comparison})
}

fn provenance(value: &grapt_types::UsageProvenance) -> Value {
    use grapt_types::{UsageDerivation, UsageProvenance};
    match value {
        UsageProvenance::ProviderReported => json!({"kind":"provider-reported"}),
        UsageProvenance::ProviderDerived { rule } => {
            json!({"kind":"provider-derived","rule":match rule {
            UsageDerivation::InputFromPromptAndToolUse=>"input-from-prompt-and-tool-use",
            UsageDerivation::OutputFromTotalLessInput=>"output-from-total-less-input",
            UsageDerivation::FreshFromInclusiveInput=>"fresh-from-inclusive-input",
            UsageDerivation::InclusiveInputFromPartition=>"inclusive-input-from-partition"}})
        }
        UsageProvenance::HarnessEstimated { policy } => {
            json!({"kind":"harness-estimated","policy":policy.as_str()})
        }
    }
}

fn subtotals(snapshot: &AccountingSnapshot) -> Result<Value, String> {
    let mut result = serde_json::Map::new();
    let names = [
        "input_total",
        "input_fresh",
        "legacy_input_ambiguous",
        "output",
        "reasoning",
        "cache_read",
        "cache_write",
        "total",
    ];
    for (index, name) in names.iter().enumerate() {
        let mut sum: Option<grapt_types::TokenCount> = None;
        let mut observed = 0;
        for attempt in &snapshot.attempts {
            let usage = &attempt.usage;
            let fields = [
                usage.input_total(),
                usage.input_fresh(),
                usage.legacy_input(),
                usage.output(),
                usage.reasoning(),
                usage.cache_read(),
                usage.cache_write(),
                usage.total(),
            ];
            if let Some(field) = fields[index] {
                sum = Some(match sum {
                    Some(prior) => prior
                        .checked_add(field.count())
                        .map_err(|e| e.to_string())?,
                    None => field.count(),
                });
                observed += 1;
            }
        }
        result.insert((*name).into(),json!({"tokens":sum.map(|v|v.value()),"observed_attempts":observed,
            "unavailable_attempts":snapshot.attempts.len()-observed,"scope":"enumerated-attempts-observed-usage-only"}));
    }
    Ok(Value::Object(result))
}

/// One interval between two durable endpoints, or `None` when either is absent.
///
/// These endpoints are neither dispatch nor first output, so `LatencySample`'s
/// prompt-phase boundaries would mislabel them. Its rules are kept: a backwards
/// pair is refused rather than clamped, and so is arithmetic overflow.
fn elapsed(start: Option<i64>, end: Option<i64>) -> Result<Option<i64>, String> {
    let (Some(start), Some(end)) = (start, end) else {
        return Ok(None);
    };
    if end < start {
        return Err(format!(
            "durable latency endpoint {end} precedes its start {start}"
        ));
    }
    end.checked_sub(start)
        .map(Some)
        .ok_or_else(|| "latency arithmetic overflow".to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use grapt_session::history::accounting::AccountingAttempt;
    use grapt_types::{
        AttemptUsage, PhysicalAttemptId, TokenCount, UsageField, UsageObservationPhase,
    };

    #[test]
    fn lua_report_keeps_absent_fields_and_missing_timing_absent() -> Result<(), String> {
        let observation = AttemptUsage::observed(
            UsageField::ABSENT,
            UsageField::ABSENT,
            UsageField::reported(TokenCount::new(0).map_err(|e| e.to_string())?),
            UsageField::ABSENT,
            UsageField::ABSENT,
            UsageField::ABSENT,
            UsageField::ABSENT,
            UsageObservationPhase::DuringStream,
        );
        let usage =
            grapt_types::usage::reconcile_usage(&[observation]).map_err(|e| e.to_string())?;
        let snapshot = AccountingSnapshot {
            session: "ses_report".parse().map_err(|_| "session")?,
            last_seq: Some(4),
            events_read: 1,
            bytes_read: 100,
            enumeration_complete: true,
            operational_coverage_complete: false,
            attempts: vec![AccountingAttempt {
                request_cache: None,
                terminal_observed: false,
                operation: AccountingOperation::Assistant,
                send_permitted: false,
                attempt: PhysicalAttemptId::new("pat_report").ok_or("attempt")?,
                step: None,
                selection: None,
                usage,
                opening: None,
                first_block: None,
                step_terminal: None,
                completed_at: None,
            }],
        };
        let result = serde_json::to_value(report(snapshot, None)?).map_err(|e| e.to_string())?;
        assert_eq!(
            result["attempts"][0]["usage"]["input_fresh"]["status"], "unavailable",
            "absence became zero"
        );
        assert_eq!(result["attempts"][0]["usage"]["output"]["tokens"], 0);
        assert_eq!(result["attempts"][0]["operation"], "assistant");
        assert!(result["attempts"][0]["latency"]["first_block_elapsed_ms"].is_null());
        assert_eq!(
            elapsed(Some(0), None)?,
            None,
            "missing timestamp resurrected"
        );
        assert_eq!(elapsed(Some(0), Some(0))?, Some(0));
        assert!(elapsed(Some(2), Some(1)).is_err());
        assert!(elapsed(Some(i64::MIN), Some(i64::MAX)).is_err());
        Ok(())
    }

    #[test]
    fn summary_usage_is_visible_without_inventing_model_cost_or_total_coverage(
    ) -> Result<(), String> {
        let field = |count| {
            TokenCount::new(count)
                .map(UsageField::reported)
                .map_err(|e| e.to_string())
        };
        let observation = AttemptUsage::observed(
            field(50)?,
            field(20)?,
            field(2)?,
            UsageField::ABSENT,
            field(30)?,
            field(0)?,
            UsageField::ABSENT,
            UsageObservationPhase::Terminal,
        );
        let snapshot = AccountingSnapshot {
            session: "ses_summary".parse().map_err(|_| "session")?,
            last_seq: Some(2),
            events_read: 3,
            bytes_read: 500,
            enumeration_complete: true,
            operational_coverage_complete: false,
            attempts: vec![AccountingAttempt {
                request_cache: None,
                terminal_observed: false,
                operation: AccountingOperation::Compaction,
                send_permitted: false,
                attempt: PhysicalAttemptId::new("summary").ok_or("attempt")?,
                step: StepNumber::new(1),
                selection: None,
                usage: grapt_types::usage::reconcile_usage(&[observation])
                    .map_err(|e| e.to_string())?,
                opening: None,
                first_block: None,
                step_terminal: None,
                completed_at: None,
            }],
        };
        let result = serde_json::to_value(report(snapshot, None)?).map_err(|e| e.to_string())?;
        assert_eq!(result["attempts"][0]["operation"], "compaction");
        assert!(result["attempts"][0]["model"].is_null());
        assert_eq!(result["attempts"][0]["cost"]["status"], "unavailable");
        assert_eq!(result["observed_subtotals"]["input_total"]["tokens"], 50);
        assert_eq!(result["observed_subtotals"]["cache_read"]["tokens"], 30);
        assert_eq!(result["session_totals"]["status"], "incomplete");
        Ok(())
    }
}

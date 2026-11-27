//! One total per dispatched attempt, rather than per usage frame or assistant row.
use grapt_provider::DispatchSample;
use grapt_types::usage::ReconciledUsage;
use grapt_types::{AttemptUsage, UsageObservationPhase};
mod cost;
#[derive(Default)]
pub(super) struct Totals {
    cost: cost::Totals,
    scope: Option<String>,
    attempts: u64,
    unknown: u64,
    input: u64,
    read: u64,
    write: u64,
    unknown_write: u64,
    overflow: bool,
}
fn add(total: &mut u64, value: u64, overflow: &mut bool) {
    match total
        .checked_add(value)
        .filter(|sum| *sum <= grapt_api::u3a::SafeTokenCount::MAX)
    {
        Some(sum) => *total = sum,
        None => *overflow = true,
    }
}
/// The attempt's usage, reconciled once it reported a terminal observation
/// within its bound; otherwise nothing is known of it.
fn reconciled(sample: &DispatchSample) -> Option<ReconciledUsage> {
    let observations: Vec<_> = sample
        .usage
        .iter()
        .take(grapt_provider::DISPATCH_USAGE_CAPACITY)
        .map(|(phase, usage)| {
            AttemptUsage::observed(
                usage.input_total().clone(),
                usage.input_fresh().clone(),
                usage.output().clone(),
                usage.reasoning().clone(),
                usage.cache_read().clone(),
                usage.cache_write().clone(),
                usage.total().clone(),
                *phase,
            )
        })
        .collect();
    (!sample.usage_overflowed
        && sample.usage.len() <= grapt_provider::DISPATCH_USAGE_CAPACITY
        && sample.terminal_observed
        && sample
            .usage
            .iter()
            .any(|(phase, _)| *phase == UsageObservationPhase::Terminal))
    .then(|| grapt_types::usage::reconcile_usage(&observations).ok())
    .flatten()
}

/// Input and cache-read tokens, only when both were reported and consistent.
fn input_and_read(usage: &ReconciledUsage) -> Option<(u64, u64)> {
    Some((
        usage.input_total()?.count().value(),
        usage.cache_read()?.count().value(),
    ))
    .filter(|(input, read)| read <= input)
}

/// The share of one attempt's input the provider read from its cache, from 0
/// to 1. Absent when either count was unreported; a zero input has no share.
pub(super) fn cache_read_share(sample: &DispatchSample) -> Option<f64> {
    let (input, read) = input_and_read(&reconciled(sample)?)?;
    (input > 0).then(|| read as f64 / input as f64)
}

impl Totals {
    pub(super) fn observe(&mut self, sample: &DispatchSample) {
        self.scope.get_or_insert_with(|| {
            grapt_event::canonical::canonical_hash_hex(&serde_json::Value::String(
                sample.attempt.as_str().to_owned(),
            ))
        });
        add(&mut self.attempts, 1, &mut self.overflow);
        let usage = reconciled(sample);
        self.cost.observe(sample.model.as_ref(), usage.as_ref());
        let pair = usage.as_ref().and_then(input_and_read);
        if let Some((input, read)) = pair {
            let sums = self
                .input
                .checked_add(input)
                .zip(self.read.checked_add(read))
                .filter(|(input, read)| {
                    *input <= grapt_api::u3a::SafeTokenCount::MAX
                        && *read <= grapt_api::u3a::SafeTokenCount::MAX
                });
            match sums {
                Some((input, read)) => {
                    self.input = input;
                    self.read = read;
                }
                None => self.overflow = true,
            }
        } else {
            add(&mut self.unknown, 1, &mut self.overflow);
        }
        match usage
            .as_ref()
            .and_then(|usage| usage.cache_write())
            .map(|field| field.count().value())
        {
            Some(write) => add(&mut self.write, write, &mut self.overflow),
            None => add(&mut self.unknown_write, 1, &mut self.overflow),
        }
    }
    pub(super) fn metadata(&self, lost: u64) -> serde_json::Value {
        serde_json::json!({
            "scope":self.scope, "attempts":self.attempts, "unreported":self.unknown,
            "pairedInput":self.input, "pairedRead":self.read, "knownWrite":self.write,
            "unreportedWrites":self.unknown_write, "overflow":self.overflow,
            "lostAtLeast":lost.min(grapt_api::u3a::SafeTokenCount::MAX),
            "coverage":"complete terminal input/read pairs in this capture; prior history and other Sessions excluded",
            "cost":self.cost.metadata(lost, self.overflow),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use grapt_provider::TokenUsage;
    use grapt_types::{PhysicalAttemptId, TokenCount, UsageField};
    fn usage(input: u64, read: Option<u64>) -> Result<TokenUsage, String> {
        let field = |value| {
            TokenCount::new(value)
                .map(UsageField::reported)
                .map_err(|error| error.to_string())
        };
        Ok(TokenUsage::new(
            field(input)?,
            UsageField::ABSENT,
            UsageField::ABSENT,
            UsageField::ABSENT,
            read.map(field).transpose()?.unwrap_or(UsageField::ABSENT),
            field(0)?,
            UsageField::ABSENT,
        ))
    }
    fn sample(
        id: &str,
        usage: Vec<(UsageObservationPhase, TokenUsage)>,
    ) -> Result<DispatchSample, String> {
        Ok(DispatchSample {
            attempt: PhysicalAttemptId::new(id).ok_or("attempt")?,
            session: None,
            request: None,
            model: None,
            at_ms: 0,
            response_head_at_ms: None,
            first_stream_byte_at_ms: None,
            http_status: Some(200),
            usage,
            usage_overflowed: false,
            usage_warning: None,
            usage_detail: None,
            terminal_observed: true,
        })
    }
    #[test]
    fn repeated_usage_observations_count_the_attempt_once() -> Result<(), String> {
        let usage = usage(100, Some(70))?;
        let mut totals = Totals::default();
        totals.observe(&sample(
            "call",
            vec![
                (UsageObservationPhase::DuringStream, usage.clone()),
                (UsageObservationPhase::Terminal, usage),
            ],
        )?);
        let metadata = totals.metadata(0);
        assert_eq!(metadata["attempts"], 1);
        assert_eq!(metadata["pairedInput"], 100);
        assert_eq!(metadata["pairedRead"], 70);
        assert_eq!(metadata["knownWrite"], 0);
        assert_eq!(metadata["unreported"], 0);
        assert_eq!(metadata["cost"]["status"], "unavailable");
        assert_eq!(metadata["cost"]["missingUsage"], 1);
        Ok(())
    }
    #[test]
    fn missing_zero_and_incomplete_observations_remain_distinct() -> Result<(), String> {
        let mut totals = Totals::default();
        totals.observe(&sample(
            "zero",
            vec![(UsageObservationPhase::Terminal, usage(100, Some(0))?)],
        )?);
        assert_eq!(totals.metadata(0)["unreported"], 0);
        totals.observe(&sample(
            "missing",
            vec![(UsageObservationPhase::Terminal, usage(100, None)?)],
        )?);
        totals.observe(&sample("refused", vec![])?);
        totals.observe(&sample(
            "partial",
            vec![(UsageObservationPhase::DuringStream, usage(100, Some(90))?)],
        )?);
        let mut overbound = sample(
            "overflow",
            vec![(UsageObservationPhase::Terminal, usage(100, Some(90))?)],
        )?;
        overbound.usage_overflowed = true;
        totals.observe(&overbound);
        let metadata = totals.metadata(1);
        assert_eq!(metadata["attempts"], 5);
        assert_eq!(metadata["unreported"], 4);
        assert_eq!(metadata["pairedInput"], 100);
        assert_eq!(metadata["pairedRead"], 0);
        assert_eq!(metadata["lostAtLeast"], 1);
        assert_eq!(metadata["unreportedWrites"], 3);
        Ok(())
    }
    #[test]
    fn aggregate_overflow_never_enters_the_wire_as_an_unsafe_number() -> Result<(), String> {
        let mut totals = Totals::default();
        let count = grapt_api::u3a::SafeTokenCount::MAX;
        for id in ["first", "second"] {
            totals.observe(&sample(
                id,
                vec![(UsageObservationPhase::Terminal, usage(count, Some(count))?)],
            )?);
        }
        let metadata = totals.metadata(u64::MAX);
        assert_eq!(metadata["overflow"], true);
        assert_eq!(metadata["pairedInput"], count);
        assert_eq!(metadata["pairedRead"], count);
        assert_eq!(metadata["lostAtLeast"], count);
        let mut asymmetric = Totals::default();
        asymmetric.observe(&sample(
            "large",
            vec![(
                UsageObservationPhase::Terminal,
                usage(count - 100, Some(count - 100))?,
            )],
        )?);
        asymmetric.observe(&sample(
            "small",
            vec![(UsageObservationPhase::Terminal, usage(150, Some(100))?)],
        )?);
        let metadata = asymmetric.metadata(0);
        assert_eq!(metadata["overflow"], true);
        assert_eq!(metadata["pairedInput"], count - 100);
        assert_eq!(metadata["pairedRead"], count - 100);
        Ok(())
    }
    #[test]
    fn attribution_deduplication_and_loss_do_not_depend_on_a_request_fingerprint(
    ) -> Result<(), String> {
        use crate::dogfood::continuation::admission::SessionCalibrations;
        let sessions = SessionCalibrations::default();
        let session: grapt_types::SessionId = "ses_observed_calls"
            .parse()
            .map_err(|error| format!("{error}"))?;
        let mut observed = sample(
            "observed",
            vec![(UsageObservationPhase::Terminal, usage(100, Some(70))?)],
        )?;
        observed.session = Some(session.clone());
        sessions.observe_request(&observed, 0);
        sessions.observe_request(&observed, 0);
        let before = sessions
            .request_observation(&session, &observed.attempt)
            .ok_or("usage not projected without a request fingerprint")?;
        assert_eq!(before["cacheObservedCalls"]["attempts"], 1);
        assert_eq!(before["cacheObservedCalls"]["pairedRead"], 70);
        sessions.note_dispatch_loss(2);
        let after = sessions
            .request_observation(&session, &observed.attempt)
            .ok_or("observation missing")?;
        assert_eq!(after["cacheObservedCalls"]["lostAtLeast"], 2);
        let other: grapt_types::SessionId = "ses_other_calls"
            .parse()
            .map_err(|error| format!("{error}"))?;
        assert!(sessions
            .request_observation(&other, &observed.attempt)
            .is_none());
        Ok(())
    }
    #[test]
    fn cache_read_share_and_prefix_reuse_follow_each_turn() -> Result<(), String> {
        use super::super::Observation;
        use grapt_provider::RequestDelta;
        let turn = |change, input, read| -> Result<serde_json::Value, String> {
            let sample = sample(
                "turn",
                vec![(UsageObservationPhase::Terminal, usage(input, read)?)],
            )?;
            Ok(Observation {
                change,
                previous: None,
                body_bytes: None,
                tools_bytes: None,
                instructions_bytes: None,
                usage_warning: None,
                cache_read_share: cache_read_share(&sample),
            }
            .metadata())
        };
        let unchanged = turn(Some(RequestDelta::AppendOnly), 1_000, Some(900))?;
        assert_eq!(unchanged["localPrefixUnchanged"], true);
        assert_eq!(unchanged["cacheReadShare"], 0.9);
        let changed = turn(
            Some(RequestDelta::InputChanged { index: 2 }),
            1_000,
            Some(0),
        )?;
        assert_eq!(changed["localPrefixUnchanged"], false);
        assert_eq!(changed["cacheReadShare"], 0.0);
        // Nothing to compare and an unreported read are absent, not false or zero.
        let first = turn(Some(RequestDelta::FirstObservation), 1_000, None)?;
        assert!(first["localPrefixUnchanged"].is_null());
        assert!(first["cacheReadShare"].is_null());
        Ok(())
    }
}

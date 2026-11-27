//! Volatile dispatch timing and refusals, separate from the durable permission
//! to send.
use grapt_types::{ClockAuthority, PhysicalAttemptId};
use std::sync::{
    atomic::{AtomicU64, Ordering},
    mpsc::{sync_channel, Receiver, SyncSender, TryRecvError},
    Arc,
};

/// Fixed diagnostic retention; saturation never delays a provider request.
pub const DISPATCH_SAMPLE_CAPACITY: usize = 128;
/// Per-attempt usage diagnostics; overflow remains explicitly incomplete.
pub const DISPATCH_USAGE_CAPACITY: usize = 64;

#[derive(Debug)]
pub struct DispatchSample {
    pub attempt: PhysicalAttemptId,
    pub session: Option<grapt_types::SessionId>,
    pub request: Option<crate::RequestFingerprint>,
    /// Exact dispatched model, never inferred from a later assistant or selection.
    pub model: Option<grapt_types::ModelRef>,
    pub at_ms: i64,
    /// Every decoded usage observation, including failed/retried and summary
    /// calls. Missing usage stays absent; this is not a billing ledger.
    pub usage: Vec<(grapt_types::UsageObservationPhase, crate::TokenUsage)>,
    pub usage_overflowed: bool,
    /// Closed accounting warning; successful output remains a normal completion.
    pub usage_warning: Option<grapt_types::failure_diagnostic::UsageDiagnostic>,
    /// First invalid usage object only, masked and byte-capped; no content frames.
    pub usage_detail: Option<super::terminal_detail::MaskedTerminalFrame>,
    /// A provider terminal or failure was decoded. Drop alone proves neither.
    pub terminal_observed: bool,
    /// HTTP headers observed by the transport; absent before a response.
    pub response_head_at_ms: Option<i64>,
    /// First nonempty successful response chunk observed before decoding.
    /// Polling/scheduling can delay observation; this is not wire arrival.
    pub first_stream_byte_at_ms: Option<i64>,
    /// Scalar status only; response headers and content never enter this feed.
    pub http_status: Option<u16>,
}

/// One attempt owns its timestamps until it ends. Stamping allocates nothing;
/// terminal/drop publishes one sample without waiting for diagnostic capacity.
#[derive(Debug)]
pub(crate) struct DispatchTiming {
    observer: DispatchObserver,
    sample: Option<DispatchSample>,
}

impl DispatchTiming {
    pub(crate) fn usage_warning(
        &mut self,
        warning: grapt_types::failure_diagnostic::UsageDiagnostic,
        detail: super::terminal_detail::MaskedTerminalFrame,
    ) {
        if let Some(sample) = &mut self.sample {
            sample.usage_warning = Some(warning);
            sample.usage_detail = Some(detail);
        }
    }

    pub(crate) fn request(
        &mut self,
        session: Option<grapt_types::SessionId>,
        request: Option<crate::RequestFingerprint>,
        model: grapt_types::ModelRef,
    ) {
        if let Some(sample) = &mut self.sample {
            sample.session = session;
            sample.request = request;
            sample.model = Some(model);
        }
    }

    pub(crate) fn observe(&mut self, fragment: &crate::AttemptFragment) {
        let Some(sample) = &mut self.sample else {
            return;
        };
        let observation = match fragment {
            crate::AttemptFragment::Usage { usage } => {
                Some((grapt_types::UsageObservationPhase::DuringStream, usage))
            }
            crate::AttemptFragment::ProviderTerminal { usage, .. } => {
                sample.terminal_observed = true;
                usage
                    .as_ref()
                    .map(|usage| (grapt_types::UsageObservationPhase::Terminal, usage))
            }
            crate::AttemptFragment::ProviderFailure { .. } => {
                sample.terminal_observed = true;
                None
            }
            _ => None,
        };
        if let Some((phase, usage)) = observation {
            if sample.usage.len() < DISPATCH_USAGE_CAPACITY {
                sample.usage.push((phase, usage.clone()));
            } else {
                sample.usage_overflowed = true;
            }
        }
    }

    pub(crate) fn refused(
        &self,
        status: u16,
        error_type: Option<VendorCode>,
        error_code: Option<VendorCode>,
    ) {
        if let Some(sample) = &self.sample {
            self.observer.refused(DispatchRefusal {
                attempt: sample.attempt.clone(),
                status,
                error_type,
                error_code,
            });
        }
    }

    pub(crate) fn response_head(&mut self, status: u16) {
        if let Some(sample) = self.sample.as_mut() {
            sample.response_head_at_ms = Some(self.observer.clock.now().as_millis());
            sample.http_status = Some(status);
        }
    }

    pub(crate) fn first_stream_byte(&mut self) {
        if let Some(sample) = self.sample.as_mut() {
            if sample.first_stream_byte_at_ms.is_none() {
                sample.first_stream_byte_at_ms = Some(self.observer.clock.now().as_millis());
            }
        }
    }
}

impl Drop for DispatchTiming {
    fn drop(&mut self) {
        if let Some(sample) = self.sample.take() {
            if self.observer.sender.try_send(sample).is_err() {
                self.observer.lose();
            }
        }
    }
}

/// A provider's own machine word for an error (`error.type` or `error.code`),
/// kept only when it is one this crate already knows: a documented error
/// type or code from the routes it serves, or an integer HTTP status code.
/// Any other value is recorded as `unlisted`, never as itself. A
/// provider-controlled string, however short, can carry request or account
/// text, and the master requires a diagnostic to reveal "neither payload nor
/// secret-dependent length"; every value written here comes from this list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VendorCode(String);

impl VendorCode {
    /// Documented Anthropic and OpenAI-family error types and codes.
    const KNOWN: &'static [&'static str] = &[
        "api_error",
        "authentication_error",
        "billing_error",
        "context_length_exceeded",
        "credit_balance_exhausted",
        "insufficient_quota",
        "invalid_api_key",
        "invalid_request_error",
        "invalid_value",
        "model_not_found",
        "not_found_error",
        "organization_spend_limit_exceeded",
        "organization_usage_limit_exceeded",
        "overloaded_error",
        "permission_error",
        "project_spend_limit_exceeded",
        "rate_limit_error",
        "rate_limit_exceeded",
        "request_too_large",
        "server_error",
        "subscription_sharing_usage_limit_exceeded",
        "timeout_error",
        "unsupported_parameter",
        "unsupported_value",
        "usage_limit_reached",
    ];
    const UNLISTED: &'static str = "unlisted";

    /// A known word or an HTTP status code as itself; any other string or
    /// number as `unlisted`; anything else, or nothing, as absent.
    pub(crate) fn from_json(value: &serde_json::Value) -> Option<Self> {
        let known = match value {
            serde_json::Value::String(text) => Self::KNOWN
                .iter()
                .find(|known| **known == text.as_str())
                .map(|known| (*known).to_owned()),
            serde_json::Value::Number(number) => number
                .as_u64()
                .filter(|code| (100..=599).contains(code))
                .map(|code| code.to_string()),
            _ => return None,
        };
        Some(Self(known.unwrap_or_else(|| Self::UNLISTED.to_owned())))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// A provider's non-success answer to one dispatched attempt: its HTTP status
/// and, from a JSON error body, only the vendor's machine words.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DispatchRefusal {
    pub attempt: PhysicalAttemptId,
    pub status: u16,
    pub error_type: Option<VendorCode>,
    pub error_code: Option<VendorCode>,
}

#[derive(Clone)]
pub struct DispatchObserver {
    clock: Arc<dyn ClockAuthority + Send + Sync>,
    sender: SyncSender<DispatchSample>,
    refusals: SyncSender<DispatchRefusal>,
    lost: Arc<AtomicU64>,
}

impl std::fmt::Debug for DispatchObserver {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DispatchObserver").finish_non_exhaustive()
    }
}

#[derive(Debug)]
pub struct DispatchSamples {
    receiver: Receiver<DispatchSample>,
    refusals: Receiver<DispatchRefusal>,
    lost: Arc<AtomicU64>,
}

impl DispatchObserver {
    pub fn bounded(clock: Arc<dyn ClockAuthority + Send + Sync>) -> (Self, DispatchSamples) {
        let (sender, receiver) = sync_channel(DISPATCH_SAMPLE_CAPACITY);
        let (refused, refusals) = sync_channel(DISPATCH_SAMPLE_CAPACITY);
        let lost = Arc::new(AtomicU64::new(0));
        (
            Self {
                clock,
                sender,
                refusals: refused,
                lost: Arc::clone(&lost),
            },
            DispatchSamples {
                receiver,
                refusals,
                lost,
            },
        )
    }

    pub(crate) fn dispatched(self, attempt: PhysicalAttemptId) -> DispatchTiming {
        let sample = DispatchSample {
            attempt,
            session: None,
            request: None,
            model: None,
            at_ms: self.clock.now().as_millis(),
            usage: Vec::new(),
            usage_overflowed: false,
            usage_warning: None,
            usage_detail: None,
            terminal_observed: false,
            response_head_at_ms: None,
            first_stream_byte_at_ms: None,
            http_status: None,
        };
        DispatchTiming {
            observer: self,
            sample: Some(sample),
        }
    }

    pub(crate) fn refused(&self, refusal: DispatchRefusal) {
        if self.refusals.try_send(refusal).is_err() {
            self.lose();
        }
    }

    fn lose(&self) {
        let _ = self
            .lost
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |old| {
                Some(old.saturating_add(1))
            });
    }
}

impl DispatchSamples {
    pub fn try_next(&self) -> Result<DispatchSample, TryRecvError> {
        self.receiver.try_recv()
    }

    pub fn try_next_refusal(&self) -> Result<DispatchRefusal, TryRecvError> {
        self.refusals.try_recv()
    }

    /// Cumulative lower bound if the counter saturated; never reset by a drain.
    pub fn lost_at_least(&self) -> u64 {
        self.lost.load(Ordering::Relaxed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[derive(Debug)]
    struct Clock;
    impl ClockAuthority for Clock {
        fn now(&self) -> grapt_types::Timestamp {
            grapt_types::Timestamp::from_millis(0)
        }
    }
    #[test]
    fn usage_retention_is_bounded_and_missing_counts_stay_absent() -> Result<(), String> {
        let (observer, samples) = DispatchObserver::bounded(Arc::new(Clock));
        let attempt = PhysicalAttemptId::new("usage_bound").ok_or("attempt")?;
        let mut timing = observer.dispatched(attempt);
        let fragment = crate::AttemptFragment::Usage {
            usage: crate::TokenUsage::default(),
        };
        for _ in 0..65 {
            timing.observe(&fragment);
        }
        timing.observe(&crate::AttemptFragment::ProviderTerminal {
            finish: crate::FinishReason::Stop,
            usage: None,
            state: crate::AdapterState::none(),
        });
        drop(timing);
        let sample = samples.try_next().map_err(|error| error.to_string())?;
        assert_eq!(sample.usage.len(), 64);
        assert!(sample.usage_overflowed);
        assert!(sample.terminal_observed);
        assert!(sample.usage.iter().all(|(phase, usage)| {
            *phase == grapt_types::UsageObservationPhase::DuringStream
                && usage.cache_read().value().is_none()
        }));
        Ok(())
    }

    #[test]
    fn only_a_known_word_or_status_is_kept_as_itself() {
        for (value, kept) in [
            (
                serde_json::json!("invalid_request_error"),
                Some("invalid_request_error"),
            ),
            (
                serde_json::json!("context_length_exceeded"),
                Some("context_length_exceeded"),
            ),
            (serde_json::json!(400), Some("400")),
            (serde_json::json!(599), Some("599")),
            // Provider-controlled values that merely look like codes.
            (serde_json::json!("sk-secret-xyz"), Some("unlisted")),
            (
                serde_json::json!("key_sk-secret-xyz_is_bad"),
                Some("unlisted"),
            ),
            (serde_json::json!("tool.schema"), Some("unlisted")),
            (serde_json::json!("Invalid Request"), Some("unlisted")),
            (serde_json::json!(""), Some("unlisted")),
            (serde_json::json!(4_000_123), Some("unlisted")),
            (serde_json::json!(-1), Some("unlisted")),
            (serde_json::json!(1.5), Some("unlisted")),
            (serde_json::json!(true), None),
            (serde_json::json!(null), None),
            (serde_json::json!({"type": "nested"}), None),
        ] {
            assert_eq!(
                VendorCode::from_json(&value)
                    .as_ref()
                    .map(VendorCode::as_str),
                kept,
                "{value}"
            );
        }
    }

    #[test]
    fn a_saturated_diagnostic_reports_loss_and_preserves_zero_time() -> Result<(), String> {
        let (observer, samples) = DispatchObserver::bounded(Arc::new(Clock));
        let attempt = PhysicalAttemptId::new("diagnostic").ok_or("attempt")?;
        for _ in 0..DISPATCH_SAMPLE_CAPACITY + 1 {
            drop(observer.clone().dispatched(attempt.clone()));
        }
        assert_eq!(samples.lost_at_least(), 1);
        for _ in 0..DISPATCH_SAMPLE_CAPACITY {
            let sample = samples.try_next().map_err(|error| error.to_string())?;
            assert_eq!(sample.attempt, attempt);
            assert_eq!(sample.at_ms, 0);
            assert_eq!(sample.response_head_at_ms, None);
            assert_eq!(sample.first_stream_byte_at_ms, None);
        }
        assert!(matches!(samples.try_next(), Err(TryRecvError::Empty)));
        drop(observer);
        assert!(matches!(
            samples.try_next(),
            Err(TryRecvError::Disconnected)
        ));
        Ok(())
    }

    #[test]
    fn observed_zero_phases_and_dropped_receiver_are_distinct_from_absence() -> Result<(), String> {
        let (observer, samples) = DispatchObserver::bounded(Arc::new(Clock));
        let attempt = PhysicalAttemptId::new("zero_phase").ok_or("attempt")?;
        let mut timing = observer.clone().dispatched(attempt.clone());
        timing.response_head(200);
        timing.first_stream_byte();
        drop(timing);
        let sample = samples.try_next().map_err(|e| e.to_string())?;
        assert_eq!(sample.response_head_at_ms, Some(0));
        assert_eq!(sample.first_stream_byte_at_ms, Some(0));
        assert_eq!(sample.http_status, Some(200));
        drop(samples);
        drop(observer.clone().dispatched(attempt));
        assert_eq!(observer.lost.load(Ordering::Relaxed), 1);
        Ok(())
    }
}

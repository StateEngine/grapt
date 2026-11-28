//! Conservative request projection; never provider-confirmed tokenizer truth.
mod persisted;
mod request_diagnostics;
mod summary;
mod usage;
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
pub(in crate::dogfood) use usage::{auto_compact_reached, ceiling_size, confirmed_usage_in};

use grapt_provider::{CodexInputItem, ProviderRequest};
use grapt_types::{ContextFailure, SessionId};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::dogfood) enum EstimateBasis {
    /// Provider-confirmed input for the exact prior request, plus a local
    /// estimate for the new suffix.
    ConfirmedPrefix,
    /// A local estimate scaled by this target's own confirmed usage.
    Calibrated,
    /// The uncalibrated byte bound.
    Estimated,
    StructuralWindow,
}

impl EstimateBasis {
    /// The durable descriptor records provenance, not method: a calibrated
    /// count is still a local estimate, so it keeps the existing vocabulary and
    /// no older reader is handed an unknown basis.
    pub(in crate::dogfood) const fn name(self) -> &'static str {
        match self {
            Self::ConfirmedPrefix => "confirmed-prefix",
            Self::Calibrated | Self::Estimated => "estimated",
            Self::StructuralWindow => "structural-window",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::dogfood) struct AdmissionDiagnosis {
    pub trigger: grapt_types::CompactionTrigger,
    pub estimate_basis: EstimateBasis,
    pub estimate: u64,
    pub budget: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::dogfood) struct Estimate {
    /// The request at this target's observed confirmed-per-estimated ratio,
    /// with no margin. Every admission decision reads this number: the window
    /// fit, the configured threshold, the project's 90% ceiling, the recorded
    /// coding-request count and therefore the summary budget.
    ///
    /// It is a local estimate, so it can under-count when content changed
    /// since the last observed request; the provider's own overflow
    /// classification recovers that with the Step's single rebuild. Charging
    /// the margin here instead compacted a session at about half its real
    /// usage and refused the cache-warm summary that would have fit;
    /// the margin survives only as the conservative payload bound in
    /// [`Calibration::charge`].
    pub tokens: u64,
    pub basis: EstimateBasis,
}

#[derive(Clone, Default)]
pub(in crate::dogfood) struct DiagnosisSlot(std::sync::Arc<std::sync::Mutex<DiagnosisState>>);

impl std::fmt::Debug for DiagnosisSlot {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("DiagnosisSlot")
    }
}

#[derive(Clone, Default)]
struct DiagnosisState {
    diagnosis: Option<AdmissionDiagnosis>,
    request: Option<CodingRequest>,
    /// The same-target calibration admission used, so the rebuilt request is
    /// judged by the rule that triggered its compaction.
    calibration: Option<Calibration>,
    encoding: Option<grapt_provider::RequestEncoding>,
    /// The request and calibration the previous plan sent, kept for one plan
    /// so a structural refusal, which plans no request, can stand on it.
    previous: Option<(CodingRequest, Option<Calibration>)>,
    /// What this plan's request can carry, read before its history.
    planned: Option<PlannedCoverage>,
    /// Whether `request` was admitted to be sent. Only a sent request is the
    /// prefix the provider holds; one refused at the threshold, or one reused
    /// for a refusal, is not kept.
    sent: bool,
}

/// The history a coding request can carry: nothing after the Session head
/// its plan read at, and nothing from the next input's ceiling on, which its
/// own context stops before.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::dogfood) struct PlannedCoverage {
    pub(in crate::dogfood) head: u64,
    pub(in crate::dogfood) ceiling: Option<u64>,
}

#[derive(Clone)]
struct CodingRequest {
    request: ProviderRequest,
    history_start: usize,
    /// Admission's token charge, replaced by observed input once it is sent.
    tokens: Option<u64>,
    /// [`DiagnosisState::planned`] of the plan that built it.
    coverage: Option<PlannedCoverage>,
}

pub(in crate::dogfood) struct RequestSnapshot {
    request: Option<CodingRequest>,
    calibration: Option<Calibration>,
    encoding: Option<grapt_provider::RequestEncoding>,
}

impl DiagnosisSlot {
    pub(in crate::dogfood) fn request_snapshot(&self) -> RequestSnapshot {
        let state = super::lock(&self.0);
        RequestSnapshot {
            request: state.request.clone(),
            calibration: state.calibration.clone(),
            encoding: state.encoding.clone(),
        }
    }

    pub(in crate::dogfood) fn restore_request(&self, snapshot: RequestSnapshot) {
        let mut state = super::lock(&self.0);
        state.request = snapshot.request;
        state.calibration = snapshot.calibration;
        state.encoding = snapshot.encoding;
    }

    pub(in crate::dogfood) fn clear(&self) {
        let mut state = super::lock(&self.0);
        let previous = if state.sent {
            let calibration = state.calibration.take();
            state.request.take().map(|request| (request, calibration))
        } else {
            None
        };
        *state = DiagnosisState {
            previous,
            ..DiagnosisState::default()
        };
    }

    /// A structural refusal returns before any coding request is planned, so
    /// the compactor had none to extend: on row A all 12 compactions fell back
    /// from the provider's server-side compaction ("no coding request") and
    /// from the cache-warm summary to a cold one that read nothing from cache
    /// (SC-COMPACT-01). The request the previous plan recorded is the one the
    /// provider last received, and the summary window's cut never passes the
    /// opening of the response it produced, so it stands for that window. It
    /// serves this one refusal and is not kept past the next plan.
    pub(in crate::dogfood) fn reuse_previous_request(&self) {
        let mut state = super::lock(&self.0);
        if state.request.is_some() {
            return;
        }
        if let Some((request, calibration)) = state.previous.take() {
            state.request = Some(request);
            state.calibration = calibration;
        }
    }

    /// The planned request was admitted and goes to the provider.
    pub(in crate::dogfood) fn sent(&self) {
        super::lock(&self.0).sent = true;
    }

    pub(in crate::dogfood) fn record(&self, diagnosis: AdmissionDiagnosis) {
        super::lock(&self.0).diagnosis = Some(diagnosis);
    }

    pub(in crate::dogfood) fn record_encoding(&self, encoding: grapt_provider::RequestEncoding) {
        super::lock(&self.0).encoding = Some(encoding);
    }

    pub(in crate::dogfood) fn record_calibration(&self, calibration: Option<Calibration>) {
        super::lock(&self.0).calibration = calibration;
    }

    pub(in crate::dogfood) fn record_request(
        &self,
        request: &ProviderRequest,
        history_start: usize,
    ) -> Result<(), ContextFailure> {
        if history_start > input(request).len() {
            return Err(ContextFailure::CompactionFailed);
        }
        let mut state = super::lock(&self.0);
        let coverage = state.planned;
        state.request = Some(CodingRequest {
            request: request.clone(),
            history_start,
            tokens: None,
            coverage,
        });
        Ok(())
    }

    /// The admitted count this plan judged its request by, never a margin
    /// above it: a margin-inflated number is not usage and must not stand in
    /// as the coding request's count for the warm summary budget.
    pub(in crate::dogfood) fn record_tokens(&self, count: u64, previous_input: Option<u64>) {
        let mut state = super::lock(&self.0);
        if let Some(request) = &mut state.request {
            request.tokens = Some(count);
        }
        if let (Some((request, _)), Some(tokens)) = (&mut state.previous, previous_input) {
            request.tokens = Some(tokens);
        }
    }

    /// A large new tool result can exceed even the compaction window. Use
    /// the last sent prefix instead; its coverage keeps the unseen tail exact.
    pub(in crate::dogfood) fn retain_fitting_request(
        &self,
        budget: u64,
    ) -> Result<(), &'static str> {
        let mut state = super::lock(&self.0);
        if state.request.is_none() && state.previous.is_none() {
            return Err("no coding request");
        }
        let encoding = state.encoding.clone();
        let fits_request = |request: &CodingRequest, calibration: Option<&Calibration>| {
            let count = match request.tokens {
                Some(count) => count,
                None => {
                    let full = estimate(&request.request)?;
                    match calibration {
                        Some(calibration) => calibration.tokens(full)?,
                        None => encoding.as_ref().map_or(Ok(full), |encoding| {
                            encoding
                                .estimate(&request.request)
                                .map_err(|_| ContextFailure::CompactionFailed)
                        })?,
                    }
                }
            };
            Ok::<_, ContextFailure>(fits(count, SUMMARY_OUTPUT_RESERVE, budget))
        };
        if let Some(request) = &state.request {
            if fits_request(request, state.calibration.as_ref())
                .map_err(|_| "coding request accounting failed")?
            {
                return Ok(());
            }
        }
        if let Some((request, calibration)) = state.previous.take() {
            if fits_request(&request, calibration.as_ref())
                .map_err(|_| "coding request accounting failed")?
            {
                state.request = Some(request);
                state.calibration = calibration;
                return Ok(());
            }
        }
        Err("coding request exceeds provider compaction budget")
    }

    /// What this plan's request can carry, read before its history.
    pub(in crate::dogfood) fn record_coverage(&self, coverage: Option<PlannedCoverage>) {
        super::lock(&self.0).planned = coverage;
    }

    /// What the coding request could carry, which bounds what a summary
    /// standing for it may claim to cover (the SC-COMPACT-01 fix 2 review hold).
    pub(in crate::dogfood) fn covered(&self) -> Option<PlannedCoverage> {
        super::lock(&self.0)
            .request
            .as_ref()
            .and_then(|coding| coding.coverage)
    }

    /// A request with history holes cannot stand for any contiguous durable
    /// prefix. Forget it and the previous-request fallback: only a new cold
    /// summary that quotes durable history may replace that history.
    pub(in crate::dogfood) fn discard_partial_projection(&self) {
        let mut state = super::lock(&self.0);
        state.request = None;
        state.previous = None;
        state.planned = None;
    }

    /// Forget the coding request, so no summary extends it.
    pub(in crate::dogfood) fn discard_request(&self) {
        super::lock(&self.0).request = None;
    }

    /// The last coding request admission planned, as it would be sent.
    pub(in crate::dogfood) fn coding_request(&self) -> Option<ProviderRequest> {
        super::lock(&self.0)
            .request
            .as_ref()
            .map(|coding| coding.request.clone())
    }

    pub(in crate::dogfood) fn matching(
        &self,
        trigger: grapt_types::CompactionTrigger,
    ) -> Option<AdmissionDiagnosis> {
        let mut diagnosis = super::lock(&self.0).diagnosis?;
        let structural = diagnosis.estimate_basis == EstimateBasis::StructuralWindow;
        if structural != (trigger == grapt_types::CompactionTrigger::StructuralBound) {
            return None;
        }
        diagnosis.trigger = trigger;
        Some(diagnosis)
    }
}

/// The margin on a target's observed confirmed-per-estimated ratio, used only
/// to bound how much content SE packs into a budget it chooses to fill.
///
/// Measured on the pinned cohort corpus (`fixtures/context-quality/`): scaling
/// by the last observed ratio alone under-counted 116 of 222 coding requests,
/// by up to 1.53x, when content changed after a compaction; twice the ratio
/// under-counted none. Packing at that bound can only under-fill, which is
/// safe. Judging a request's size at it is not: it is not what the provider
/// charged, so it compacted a Session at about half its real usage.
/// [`Calibration::tokens`] is the judging count.
pub(in crate::dogfood) const CALIBRATION_MARGIN: u64 = 2;

const MAX_ESTIMATED_PER_CONFIRMED: u64 = 8;

/// The fewest tokens `units` uncalibrated local units can plausibly charge:
/// the count at [`MAX_ESTIMATED_PER_CONFIRMED`] units per token.
pub(in crate::dogfood) fn densest(units: u64) -> u64 {
    units.div_ceil(MAX_ESTIMATED_PER_CONFIRMED)
}

/// Provider-confirmed input per locally estimated unit, for one exact target.
#[derive(Clone)]
pub(in crate::dogfood) struct Calibration {
    target: TargetKey,
    confirmed: u64,
    estimated: u64,
}

impl Calibration {
    /// A report implying more than [`MAX_ESTIMATED_PER_CONFIRMED`] estimated
    /// units per confirmed token is not trusted: no byte-level tokenizer in the
    /// corpus exceeded about 5.3 bytes per token, so such a count more likely
    /// omits part of the input than measures it.
    fn new(target: TargetKey, confirmed: u64, estimated: u64) -> Option<Self> {
        let plausible = confirmed > 0
            && confirmed
                .checked_mul(MAX_ESTIMATED_PER_CONFIRMED)
                .is_none_or(|bound| bound >= estimated);
        plausible.then_some(Self {
            target,
            confirmed,
            estimated,
        })
    }

    /// `estimated` local units at the observed ratio itself, rounded up and
    /// never more than the uncalibrated estimate. This is what admission
    /// judges: this target's own provider-charged tokens per local unit.
    pub(in crate::dogfood) fn tokens(&self, estimated: u64) -> Result<u64, ContextFailure> {
        self.scaled(estimated, 1)
    }

    /// `estimated` local units at [`CALIBRATION_MARGIN`] times the observed
    /// ratio, rounded up and never more than the uncalibrated estimate.
    ///
    /// This bounds content SE packs into a budget it chooses to fill
    /// (a summary payload, a working set, a rebuilt request), and is the exact
    /// inverse of [`Self::within`]. It is never a request's admission size:
    /// that is [`Self::tokens`].
    pub(in crate::dogfood) fn charge(&self, estimated: u64) -> Result<u64, ContextFailure> {
        self.scaled(estimated, CALIBRATION_MARGIN)
    }

    /// The most local units whose marginal charge at [`CALIBRATION_MARGIN`]
    /// times the observed ratio stays within `tokens`, and never fewer than
    /// `tokens`, as that charge never exceeds its estimate. Used only to size
    /// content SE chooses to pack, never to judge a request the provider
    /// already accounted for.
    pub(in crate::dogfood) fn within(&self, tokens: u64) -> u64 {
        let units = u128::from(tokens)
            .saturating_mul(u128::from(self.estimated))
            .checked_div(u128::from(self.confirmed).saturating_mul(u128::from(CALIBRATION_MARGIN)))
            .unwrap_or(0);
        u64::try_from(units).unwrap_or(u64::MAX).max(tokens)
    }

    fn scaled(&self, estimated: u64, margin: u64) -> Result<u64, ContextFailure> {
        let scaled = u128::from(estimated)
            .checked_mul(u128::from(self.confirmed))
            .and_then(|value| value.checked_mul(u128::from(margin)))
            .ok_or(ContextFailure::CompactionFailed)?
            .div_ceil(u128::from(self.estimated));
        Ok(u64::try_from(scaled).map_or(estimated, |scaled| scaled.min(estimated)))
    }
}

/// The latest calibration and admitted request per Session, shared by its
/// successive executions so a new prompt does not fall back to the byte bound
/// or count its whole history from scratch. The calibration itself also
/// survives a restart (see [`persisted`]). Bounded; an evicted Session only
/// loses its accounting, never correctness.
#[derive(Clone, Default)]
pub(in crate::dogfood) struct SessionCalibrations(
    Arc<Mutex<BTreeMap<SessionId, SessionAccounting>>>,
);

#[derive(Default)]
struct SessionAccounting {
    wire: request_diagnostics::WireHistory,
    calibration: Option<Calibration>,
    previous: Option<Prior>,
    /// Whether this process already read the durable calibration.
    restored: bool,
    /// The calibration last read from or written to the Session's log.
    written: Option<grapt_session::notice::TokenCalibration>,
}

const MAX_CALIBRATED_SESSIONS: usize = 256;

impl SessionCalibrations {
    fn get(&self, session: &SessionId) -> (Option<Calibration>, Option<Prior>) {
        let sessions = super::lock(&self.0);
        sessions.get(session).map_or((None, None), |held| {
            (held.calibration.clone(), held.previous.clone())
        })
    }

    fn update(&self, session: &SessionId, change: impl FnOnce(&mut SessionAccounting)) {
        let mut sessions = super::lock(&self.0);
        if !sessions.contains_key(session) && sessions.len() >= MAX_CALIBRATED_SESSIONS {
            let _ = sessions.pop_first();
        }
        change(sessions.entry(session.clone()).or_default());
    }
}

/// What later admission needs of an admitted request, without retaining it:
/// its target, its input length, a digest of its shape and input, and its
/// local estimate.
#[derive(Clone)]
struct Prior {
    attempt: Option<grapt_types::PhysicalAttemptId>,
    target: TargetKey,
    items: usize,
    digest: [u8; 32],
    estimated: u64,
    context: Option<grapt_types::RequestContextIdentity>,
}

impl Prior {
    fn of(request: &ProviderRequest, target: TargetKey, estimated: u64) -> Option<Self> {
        let items = input(request).len();
        Some(Self {
            attempt: None,
            target,
            items,
            digest: grapt_provider::prefix_digest(request, items)?,
            estimated,
            context: None,
        })
    }

    /// Whether `request` repeats this one exactly and only adds input. A
    /// compaction, revert or edit since replaces part of that input, so the
    /// prior confirmed count would no longer describe the prefix.
    fn prefix_of(&self, request: &ProviderRequest) -> bool {
        grapt_provider::prefix_digest(request, self.items) == Some(self.digest)
    }
}

#[derive(Default)]
pub(in crate::dogfood) struct Accounting {
    previous: Option<Prior>,
    pending: Option<Prior>,
    calibration: Option<Calibration>,
    session: Option<(SessionCalibrations, SessionId)>,
    recovery_encoding: Option<grapt_provider::RequestEncoding>,
}

#[derive(Clone)]
struct TargetKey {
    model: grapt_types::ModelRef,
    route: grapt_types::ModelRouteId,
    account: grapt_types::AccountId,
    credential: grapt_types::CredentialVersion,
    policy: grapt_types::PolicyVersion,
}

impl TargetKey {
    fn new(target: &grapt_types::ResolvedModelTarget) -> Self {
        Self {
            model: target.model_ref().clone(),
            route: target.route().clone(),
            account: target.account().clone(),
            credential: target.credential_version().clone(),
            policy: target.policy().clone(),
        }
    }

    fn matches(&self, target: &grapt_types::ResolvedModelTarget) -> bool {
        self.model.is_same_issuance(target.model_ref())
            && &self.route == target.route()
            && &self.account == target.account()
            && &self.credential == target.credential_version()
            && &self.policy == target.policy()
    }
}

impl Accounting {
    pub(in crate::dogfood) fn set_recovery_encoding(
        &mut self,
        encoding: Option<grapt_provider::RequestEncoding>,
    ) {
        self.recovery_encoding = encoding;
    }
    /// Share this Session's calibration and last admitted request with its
    /// later executions, so the first Step of a new prompt is charged from
    /// the provider's confirmed count of the history it repeats.
    pub(in crate::dogfood) fn for_session(
        sessions: SessionCalibrations,
        session: SessionId,
    ) -> Self {
        let (calibration, previous) = sessions.get(&session);
        Self {
            previous,
            calibration,
            session: Some((sessions, session)),
            ..Self::default()
        }
    }

    pub(in crate::dogfood) fn previous_attempt(&self) -> Option<&grapt_types::PhysicalAttemptId> {
        self.previous
            .as_ref()
            .and_then(|previous| previous.attempt.as_ref())
    }

    /// Context identity changes on compaction; catalog/schema refreshes do not.
    pub(in crate::dogfood) fn previous_context_is_retained(
        &self,
        planned: &grapt_runtime::PlannedAttempt,
    ) -> bool {
        self.previous.as_ref().is_some_and(|prior| {
            prior.target.matches(planned.authority.model())
                && prior.context == planned.context
                && input(&planned.request).len() >= prior.items
        })
    }

    pub(in crate::dogfood) fn record_context(
        &mut self,
        context: Option<&grapt_types::RequestContextIdentity>,
    ) {
        if let Some(pending) = &mut self.pending {
            pending.context = context.cloned();
        }
    }

    /// The calibration valid for `target`, if its own usage produced one.
    pub(in crate::dogfood) fn calibration_for(
        &self,
        target: &grapt_types::ResolvedModelTarget,
    ) -> Option<Calibration> {
        self.calibration
            .clone()
            .filter(|calibration| calibration.target.matches(target))
    }

    fn calibrate(&mut self, calibration: Calibration) {
        if let Some((sessions, session)) = &self.session {
            sessions.update(session, |shared| {
                shared.calibration = Some(calibration.clone())
            });
        }
        self.calibration = Some(calibration);
    }

    pub(in crate::dogfood) fn admitted(&mut self, attempt: &grapt_types::PhysicalAttemptId) {
        self.previous = self.pending.take().map(|mut prior| {
            prior.attempt = Some(attempt.clone());
            prior
        });
        if let (Some((sessions, session)), Some(previous)) = (&self.session, &self.previous) {
            sessions.update(session, |shared| shared.previous = Some(previous.clone()));
        }
    }

    pub(in crate::dogfood) fn estimate(
        &mut self,
        request: &ProviderRequest,
        target: &grapt_types::ResolvedModelTarget,
        confirmed: Option<u64>,
    ) -> Result<Estimate, ContextFailure> {
        self.pending = None;
        let full = estimate(request)?;
        // The previous admitted request's confirmed input, on this exact
        // target, both calibrates later estimates and, when this request
        // repeats it exactly and only adds to it, replaces the estimate for
        // that prefix. The previous request may be the last one of an earlier
        // execution of this Session.
        let observed = match (&self.previous, confirmed) {
            (Some(prior), Some(confirmed)) if prior.target.matches(target) => Some((
                prior.target.clone(),
                confirmed,
                prior.estimated,
                prior.prefix_of(request),
            )),
            _ => None,
        };
        let mut prefix = None;
        if let Some((binding, confirmed, previous, exact)) = observed {
            if let Some(calibration) = Calibration::new(binding, confirmed, previous) {
                self.calibrate(calibration);
            }
            if exact {
                let suffix = full
                    .checked_sub(previous)
                    .ok_or(ContextFailure::CompactionFailed)?;
                prefix = Some((confirmed, suffix));
            }
        }
        let calibration = self.calibration_for(target);
        let (tokens, basis) = match (prefix, &calibration) {
            (Some((confirmed, suffix)), calibration) => {
                let suffix = match calibration {
                    Some(calibration) => calibration.tokens(suffix)?,
                    None => suffix,
                };
                (
                    confirmed
                        .checked_add(suffix)
                        .ok_or(ContextFailure::CompactionFailed)?,
                    EstimateBasis::ConfirmedPrefix,
                )
            }
            (None, Some(calibration)) => (calibration.tokens(full)?, EstimateBasis::Calibrated),
            (None, None) => (
                self.recovery_encoding
                    .as_ref()
                    .map_or(Ok(full), |encoding| {
                        encoding
                            .estimate(request)
                            .map_err(|_| ContextFailure::CompactionFailed)
                    })?,
                EstimateBasis::Estimated,
            ),
        };
        // Bound the disposable copy even when provider usage discounts a prefix.
        if full > super::MAX_CONTEXT_BYTES as u64 {
            return Err(ContextFailure::CompactionFailed);
        }
        self.pending = Prior::of(request, TargetKey::new(target), full);
        Ok(Estimate { tokens, basis })
    }
}

pub(in crate::dogfood) fn input(request: &ProviderRequest) -> &[CodexInputItem] {
    match request {
        ProviderRequest::OpenAiResponses(request) => &request.input,
        ProviderRequest::ChatGptCodex(request) => &request.input,
        ProviderRequest::AnthropicMessages(request) | ProviderRequest::VertexAnthropic(request) => {
            &request.messages
        }
        ProviderRequest::OpenAiCompatible(request) | ProviderRequest::WatsonxChat(request) => {
            &request.messages
        }
        ProviderRequest::BedrockConverse(request) => &request.messages,
        ProviderRequest::GoogleGemini(request) | ProviderRequest::VertexGemini(request) => {
            &request.messages
        }
        ProviderRequest::CohereChat(request) => &request.messages,
    }
}

pub(in crate::dogfood) fn input_mut(request: &mut ProviderRequest) -> &mut Vec<CodexInputItem> {
    match request {
        ProviderRequest::OpenAiResponses(request) => &mut request.input,
        ProviderRequest::ChatGptCodex(request) => &mut request.input,
        ProviderRequest::AnthropicMessages(request) | ProviderRequest::VertexAnthropic(request) => {
            &mut request.messages
        }
        ProviderRequest::OpenAiCompatible(request) | ProviderRequest::WatsonxChat(request) => {
            &mut request.messages
        }
        ProviderRequest::BedrockConverse(request) => &mut request.messages,
        ProviderRequest::GoogleGemini(request) | ProviderRequest::VertexGemini(request) => {
            &mut request.messages
        }
        ProviderRequest::CohereChat(request) => &mut request.messages,
    }
}

impl super::DurableContinuation {
    /// Whether a user-requested `/compact` is pending. It runs as the first
    /// compaction of the next input's first Step (CODING-QUALITY-01 /compact,
    /// option iii), whatever the context size.
    pub(in crate::dogfood) fn compaction_requested(&self) -> Result<bool, ContextFailure> {
        self.read_projection(|reader| {
            super::lock(&reader.inbox)
                .pending_compaction(&reader.session)
                .map(|pending| pending.is_some())
                .map_err(|error| error.to_string())
        })
        .map_err(|_: String| ContextFailure::CompactionFailed)
    }
}

pub(in crate::dogfood) const OUTPUT_RESERVE: u32 = 16_384;

/// A summary request's output reserve: the summary's own output bound.
const SUMMARY_OUTPUT_RESERVE: u32 = 8192;
pub(in crate::dogfood) const HEADROOM: u64 = 4096;

pub(in crate::dogfood) fn estimate(request: &ProviderRequest) -> Result<u64, ContextFailure> {
    let items = input(request);
    // Fixed envelope and per-item framing are charged in addition to UTF-8
    // bytes. This is an estimate for byte tokenizers, not observed usage.
    let mut count = 256u64;
    for item in items {
        count = count
            .checked_add(item_charge(item)?)
            .ok_or(ContextFailure::CompactionFailed)?;
    }
    if let ProviderRequest::AnthropicMessages(request) = request {
        if let Some(suffix) = request
            .uncached_system_suffix
            .as_deref()
            .filter(|text| !text.is_empty())
        {
            count = add(count, suffix.len(), 64)?;
        }
    }
    if let Some(search) = request.web_search() {
        count = add(count, search.encoded_bytes(), 128)?;
    }
    if let Some(search) = request.anthropic_web_search() {
        count = add(count, search.encoded_bytes(), 128)?;
    }
    for tool in request.tools() {
        let bytes = tool
            .name
            .len()
            .saturating_add(tool.description.as_ref().map_or(0, String::len))
            .saturating_add(tool.input_schema.len());
        let bytes = match &tool.input_format {
            grapt_provider::ToolInputFormat::Json => bytes,
            grapt_provider::ToolInputFormat::Text { grammar } => {
                bytes.saturating_add(grammar.len())
            }
        };
        count = add(count, bytes, 128)?;
    }
    Ok(count)
}

/// One history item's local admission units, including framing, identities
/// and image tokens. Working-set retention must charge the same item.
pub(super) fn item_charge(item: &CodexInputItem) -> Result<u64, ContextFailure> {
    if matches!(item, CodexInputItem::AssistantResponseStart) {
        return Ok(0);
    }
    let mut count = 0;
    let bytes = match item {
        CodexInputItem::EffortUpdate(value) => value.as_str().len(),
        CodexInputItem::Message(message) => message.content.len(),
        CodexInputItem::ImageUser {
            text: output,
            images,
        }
        | CodexInputItem::ImageToolOutput { output, images, .. } => {
            count = add(count, 0, images.estimated_tokens())?;
            output.as_str().len()
        }
        CodexInputItem::CompatibleReasoning(text) => text.as_str().len(),
        CodexInputItem::ProviderReplay(replay) => replay.byte_len(),
        CodexInputItem::ToolCall {
            call,
            tool,
            arguments,
        }
        | CodexInputItem::TextToolCall {
            call,
            tool,
            input: arguments,
        } => call
            .as_str()
            .len()
            .saturating_add(tool.as_str().len())
            .saturating_add(arguments.as_str().len()),
        CodexInputItem::ToolOutput { call, output, .. } => {
            call.as_str().len().saturating_add(output.as_str().len())
        }
        CodexInputItem::AssistantResponseStart | CodexInputItem::CompactionTrigger => 0,
    };
    add(count, bytes, 64)
}

fn add(count: u64, bytes: usize, framing: u64) -> Result<u64, ContextFailure> {
    u64::try_from(bytes)
        .ok()
        .and_then(|bytes| count.checked_add(bytes))
        .and_then(|count| count.checked_add(framing))
        .ok_or(ContextFailure::CompactionFailed)
}

/// The output the request itself may produce. Codex carries no bound on the
/// wire, so the product default is reserved for it.
pub(in crate::dogfood) fn output_reserve(request: &ProviderRequest) -> u32 {
    match request {
        ProviderRequest::OpenAiResponses(request) => request
            .max_output_tokens
            .map_or(OUTPUT_RESERVE, grapt_provider::MaxOutputTokens::value),
        ProviderRequest::AnthropicMessages(request) | ProviderRequest::VertexAnthropic(request) => {
            request.max_tokens.value()
        }
        ProviderRequest::BedrockConverse(request) => request.max_tokens.value(),
        ProviderRequest::OpenAiCompatible(request) | ProviderRequest::WatsonxChat(request) => {
            request
                .max_tokens
                .map_or(OUTPUT_RESERVE, grapt_provider::MaxOutputTokens::value)
        }
        ProviderRequest::GoogleGemini(request) | ProviderRequest::VertexGemini(request) => request
            .max_tokens
            .map_or(OUTPUT_RESERVE, grapt_provider::MaxOutputTokens::value),
        ProviderRequest::CohereChat(request) => request
            .max_tokens
            .map_or(OUTPUT_RESERVE, grapt_provider::MaxOutputTokens::value),
        ProviderRequest::ChatGptCodex(_) => OUTPUT_RESERVE,
    }
}

/// Admission compares an input count with the model's advertised maximum
/// (`budget`) minus the output reserve and headroom: the operating threshold.
/// The maximum itself is never lowered.
pub(in crate::dogfood) fn fits(input: u64, reserve: u32, budget: u64) -> bool {
    input
        .checked_add(u64::from(reserve))
        .and_then(|sum| sum.checked_add(HEADROOM))
        .is_some_and(|sum| sum < budget)
}

#[cfg(test)]
#[path = "calibration_tests.rs"]
mod calibration_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use grapt_provider::{InputMessage, MessageRole, PhysicalAttemptAuthoritySnapshot};

    fn request() -> Result<ProviderRequest, String> {
        let target = crate::dogfood::production::tests::projection_target()?;
        let mut builder = PhysicalAttemptAuthoritySnapshot::builder();
        builder.model(target).map_err(|error| error.to_string())?;
        let authority = builder.build().map_err(|error| error.to_string())?;
        Ok(ProviderRequest::ChatGptCodex(
            grapt_provider::ChatGptCodexRequest {
                model: authority.model().model_ref().clone(),
                input: vec![CodexInputItem::Message(InputMessage {
                    role: MessageRole::User,
                    content: "é".repeat(2000),
                })],
                prompt_cache_key: None,
                tools: Vec::new(),
                web_search: None,
                tool_choice: None,
                reasoning: Default::default(),
                turn: Default::default(),
            },
        ))
    }

    #[test]
    fn native_search_is_charged_before_first_request_admission() -> Result<(), String> {
        let mut request = request()?;
        let before = estimate(&request).map_err(|error| format!("{error:?}"))?;
        let spec = grapt_provider::WebSearchSpec::new(grapt_provider::WebSearchAccess::Live);
        let extra = spec.encoded_bytes() as u64 + 128;
        let ProviderRequest::ChatGptCodex(codex) = &mut request else {
            return Err("fixture family".into());
        };
        codex.web_search = Some(spec);
        assert_eq!(
            estimate(&request).map_err(|error| format!("{error:?}"))?,
            before + extra
        );
        Ok(())
    }

    #[test]
    fn cr_reserve_and_headroom_apply_at_exact_threshold_and_small_budget() {
        let reserve = OUTPUT_RESERVE;
        let budget = 32_000;
        let threshold = budget - u64::from(reserve) - HEADROOM;
        assert!(fits(threshold - 1, reserve, budget));
        assert!(!fits(threshold, reserve, budget));
        assert!(!fits(threshold + 1, reserve, budget));
        assert!(!fits(0, reserve, 100));
        assert!(!fits(u64::MAX, reserve, budget));
    }

    #[test]
    fn cr_usage_discounts_only_the_exact_confirmed_prefix() -> Result<(), String> {
        let prior = request()?;
        let target = crate::dogfood::production::tests::projection_target()?;
        let raw = estimate(&prior).map_err(|error| format!("{error:?}"))?;
        assert_eq!(raw, 4320);
        let mut accounting = Accounting::default();
        assert_eq!(
            accounting.estimate(&prior, &target, Some(99)),
            Ok(Estimate {
                tokens: raw,
                basis: EstimateBasis::Estimated,
            })
        );
        let attempt = grapt_types::PhysicalAttemptId::new("cr-prefix").ok_or("attempt")?;
        accounting.admitted(&attempt);
        let mut next = prior.clone();
        let ProviderRequest::ChatGptCodex(request) = &mut next else {
            return Err("fixture family".into());
        };
        request.input.push(CodexInputItem::Message(InputMessage {
            role: MessageRole::Assistant,
            content: "done".into(),
        }));
        assert_eq!(
            accounting.estimate(&next, &target, Some(99)),
            Ok(Estimate {
                tokens: 99 + 4 + 64,
                basis: EstimateBasis::ConfirmedPrefix,
            })
        );
        assert_eq!(
            accounting.estimate(&next, &target, None),
            Ok(Estimate {
                tokens: raw + 4 + 64,
                basis: EstimateBasis::Estimated,
            })
        );
        let replacement = crate::dogfood::production::tests::projection_target()?;
        assert_eq!(
            accounting.estimate(&next, &replacement, Some(99)),
            Ok(Estimate {
                tokens: raw + 4 + 64,
                basis: EstimateBasis::Estimated,
            })
        );
        let mut binding = TargetKey::new(&target);
        binding.route = grapt_types::ModelRouteId::new("changed-route").ok_or("route")?;
        assert!(!binding.matches(&target));
        let mut binding = TargetKey::new(&target);
        binding.account = grapt_types::AccountId::new("changed-account").ok_or("account")?;
        assert!(!binding.matches(&target));
        let mut binding = TargetKey::new(&target);
        binding.credential =
            grapt_types::CredentialVersion::new("changed-credential").ok_or("credential")?;
        assert!(!binding.matches(&target));
        let mut binding = TargetKey::new(&target);
        binding.policy = grapt_types::PolicyVersion::new("changed-policy").ok_or("policy")?;
        assert!(!binding.matches(&target));
        let ProviderRequest::ChatGptCodex(request) = &mut next else {
            return Err("fixture family".into());
        };
        request.input[0] = CodexInputItem::Message(InputMessage {
            role: MessageRole::User,
            content: "superseding instruction".into(),
        });
        assert_eq!(
            accounting.estimate(&next, &target, Some(99)),
            estimate(&next).map(|tokens| Estimate {
                tokens,
                basis: EstimateBasis::Estimated,
            })
        );
        Ok(())
    }
}

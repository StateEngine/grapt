//! Summary and rebuilt-context budgeting beside the admission snapshot.
use super::*;

impl DiagnosisSlot {
    /// Why `request`, a summary request, does not fit `budget` with the
    /// summary's output reserve, or `None` when it does. It is counted as its
    /// own payload is sized: local units at the same-target calibration's
    /// conservative bound, the inverse of [`Self::summary_bytes`], so a
    /// payload that fills the window is admitted. Judging it at the observed
    /// ratio while packing at the margin would refuse every filled payload.
    /// A summary request is judged in tokens, never in raw local units, which
    /// run several times the tokens on prose-heavy history.
    pub(in crate::dogfood) fn summary_overflow(
        &self,
        request: &ProviderRequest,
        budget: u64,
    ) -> Result<Option<String>, ContextFailure> {
        let (calibration, encoding) = {
            let state = super::super::lock(&self.0);
            (state.calibration.clone(), state.encoding.clone())
        };
        let full = estimate(request)?;
        let (count, basis) = match calibration {
            Some(calibration) => (calibration.charge(full)?, "calibrated"),
            None => (
                encoding.as_ref().map_or(Ok(full), |encoding| {
                    encoding
                        .estimate(request)
                        .map_err(|_| ContextFailure::CompactionFailed)
                })?,
                "uncalibrated: no provider usage observed yet; encoded request bytes",
            ),
        };
        let reserve = if request.family().is_anthropic() {
            output_reserve(request)
        } else {
            SUMMARY_OUTPUT_RESERVE
        };
        Ok((!fits(count, reserve, budget)).then(|| {
            format!("{basis} count {count} plus output reserve exceeds the {budget}-token budget")
        }))
    }

    /// `warm_plan` appends only the summary task to our exact coding request.
    /// Keep its admitted prefix charge; re-counting the whole prefix would
    /// refuse a cache-warm summary that fits. The coding request's admitted
    /// count is judged at the observed ratio, so this extends the same number
    /// the provider confirmed rather than a margin-inflated one.
    pub(in crate::dogfood) fn warm_summary_overflow(
        &self,
        request: &ProviderRequest,
        budget: u64,
    ) -> Result<Option<String>, ContextFailure> {
        // Without measured usage the prefix is not a token observation. Count
        // this exact serialized request rather than extending a guessed prefix.
        if super::super::lock(&self.0).calibration.is_none() {
            return self.summary_overflow(request, budget);
        }
        let (count, calibrated) = {
            let state = super::super::lock(&self.0);
            let count = state
                .request
                .as_ref()
                .and_then(|coding| coding.tokens.map(|tokens| (coding, tokens)))
                .map(|(coding, tokens)| {
                    let suffix = estimate(request)?
                        .checked_sub(estimate(&coding.request)?)
                        .ok_or(ContextFailure::CompactionFailed)?;
                    // The appended summary task is new local content, so it is
                    // judged at the same observed ratio as the prefix count.
                    let suffix = state
                        .calibration
                        .as_ref()
                        .map_or(Ok(suffix), |calibration| calibration.tokens(suffix))?;
                    tokens
                        .checked_add(suffix)
                        .ok_or(ContextFailure::CompactionFailed)
                })
                .transpose()?;
            (count, state.calibration.is_some())
        };
        let Some(count) = count else {
            return self.summary_overflow(request, budget);
        };
        Ok((!fits(count, SUMMARY_OUTPUT_RESERVE, budget)).then(|| {
            let basis = if calibrated {
                "admitted prefix"
            } else {
                "uncalibrated: no provider usage observed yet"
            };
            format!("{basis} count {count} plus output reserve exceeds the {budget}-token budget")
        }))
    }

    /// The bytes a summary request's quoted history may hold in a
    /// `context_tokens` window. Before provider usage is observed that is one
    /// byte per token, as estimation charges. Once calibrated it is the
    /// calibrated charge inverted, so [`Self::summary_overflow`] still admits
    /// a payload that fills it (SC-COMPACT-01: one byte per token held the
    /// summary to about a fifth of the window on row A).
    pub(in crate::dogfood) fn summary_bytes(&self, context_tokens: u64) -> u64 {
        let tokens = context_tokens.saturating_sub(super::super::summarization::SUMMARY_RESERVE);
        super::super::lock(&self.0)
            .calibration
            .as_ref()
            .map_or(tokens, |calibration| calibration.within(tokens))
    }

    /// Local units a working set can retain within its token budget, using
    /// the same conservative calibration as the rebuilt coding request.
    pub(in crate::dogfood) fn working_set_bytes(&self, tokens: u64) -> u64 {
        super::super::lock(&self.0)
            .calibration
            .as_ref()
            .map_or(tokens, |calibration| calibration.within(tokens))
    }

    /// The rebuilt coding request's admission count and its output reserve.
    /// A rebuilt request is new content SE sizes against the window, so it is
    /// counted as [`Self::summary_bytes`] inverts: the bound it must fit under.
    pub(in crate::dogfood) fn rebuilt_estimate(
        &self,
        history: &[CodexInputItem],
    ) -> Result<Option<(u64, u32)>, ContextFailure> {
        let (request, calibration, encoding) = {
            let state = super::super::lock(&self.0);
            (
                state.request.clone(),
                state.calibration.clone(),
                state.encoding.clone(),
            )
        };
        let Some(mut request) = request else {
            return Ok(None);
        };
        let input = input_mut(&mut request.request);
        if request.history_start > input.len() {
            return Err(ContextFailure::CompactionFailed);
        }
        input.truncate(request.history_start);
        input.extend_from_slice(history);
        let full = estimate(&request.request)?;
        let count = match calibration {
            Some(calibration) => calibration.charge(full)?,
            None => encoding.as_ref().map_or(Ok(full), |encoding| {
                encoding
                    .estimate(&request.request)
                    .map_err(|_| ContextFailure::CompactionFailed)
            })?,
        };
        Ok(Some((count, output_reserve(&request.request))))
    }
}

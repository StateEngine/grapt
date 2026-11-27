//! Bounded degraded requests, dropping only complete historical exchanges.
use super::super::continuation::admission;
use super::*;

impl ProductPlan {
    pub(super) fn trim_context(
        &mut self,
        request: &mut ProviderRequest,
        boundary: &mut RequestBoundary,
    ) -> Result<bool, PlanRefusal> {
        self.trimmed_request = false;
        if !matches!(&self.context, PlanContext::Durable(continuation) if continuation.trim_requested())
        {
            return Ok(false);
        }
        // Encoded sizing sees what send carries: replays this protocol can
        // never send (a switched target's thinking) go here exactly as there,
        // and the boundary indexes follow the items they named.
        let removed = grapt_provider::drop_foreign_replays(request);
        let shift = |index: usize| index - removed.partition_point(|at| *at < index);
        boundary.history_start = shift(boundary.history_start);
        boundary.protected_from = shift(boundary.protected_from);
        let budget = self
            .targets
            .context_tokens()
            .ok_or_else(|| refusal("context trim has no model budget".into()))?;
        let reserve = u64::from(admission::output_reserve(request));
        let limit = budget
            .saturating_sub(reserve)
            .saturating_sub(admission::HEADROOM)
            .saturating_sub(1)
            .min(
                budget
                    .saturating_mul(9)
                    .saturating_div(10)
                    .saturating_sub(1),
            )
            .min(
                self.compaction
                    .model_auto_compact_token_limit
                    .unwrap_or(u64::MAX)
                    .saturating_sub(1),
            );
        let encoding = self.targets.request_encoding();
        let before = encoding.estimate(request).map_err(refusal)?;
        let window_floor = self.window_floor.take();
        if before <= limit && window_floor.is_none() {
            return Ok(false);
        }
        let after = if before > limit {
            crate::dogfood::context_projection::trim(
                request,
                boundary.history_start,
                boundary.protected_from,
                super::super::execution::context_trim::LABEL,
                &encoding,
                limit,
            )
            .map_err(refusal)?
        } else {
            before
        };
        let estimate = grapt_session::notice::ContextEstimate {
            before,
            after,
            output_reserve: reserve,
            budget,
        };
        if let Some(feedback) = &self.trim_feedback {
            feedback.record(estimate, window_floor).map_err(refusal)?;
            self.trim_feedback = None;
        }
        self.trimmed_request = true;
        Ok(true)
    }
}
pub(super) fn refusal(detail: String) -> PlanRefusal {
    PlanRefusal::new(detail, None)
}

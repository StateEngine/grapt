use super::*;
use grapt_provider::request::ChatGptCodexRequest;

pub(super) fn request_for(authority: &PhysicalAttemptAuthoritySnapshot) -> ChatGptCodexRequest {
    ChatGptCodexRequest {
        model: authority.model().model_ref().clone(),
        input: vec![grapt_provider::InputMessage {
            role: grapt_provider::MessageRole::User,
            content: "hello".into(),
        }
        .into()],
        prompt_cache_key: None,
        tools: Vec::new(),
        web_search: None,
        tool_choice: None,
        reasoning: Default::default(),
        turn: Default::default(),
    }
}

pub(super) struct RegistryPlan {
    pub(super) registry: ModelRegistry,
}

impl AttemptPlan for RegistryPlan {
    fn plan(
        &mut self,
        _step: StepNumber,
        _ordinal: u32,
        _generation: HistoryGeneration,
    ) -> Result<PlannedAttempt, PlanRefusal> {
        let authority = authority_of(issue(&self.registry));
        let request = request_for(&authority);
        Ok(PlannedAttempt {
            operation_mail: None,
            mail: None,
            context: None,
            authority,
            request: ProviderRequest::ChatGptCodex(request),
        })
    }
}

pub(super) struct StalePlan {
    pub(super) authority: Option<PhysicalAttemptAuthoritySnapshot>,
}

pub(super) struct StructuralRefusalPlan;

pub(super) struct UnavailableContinuityPlan(pub(super) grapt_session::execution::ContinuityFailure);

impl AttemptPlan for UnavailableContinuityPlan {
    fn refresh_context(
        &mut self,
        _step: StepNumber,
        _generation: HistoryGeneration,
    ) -> crate::BoxFuture<'_, Result<(), ContextPreparationFailure>> {
        let cause = self.0;
        Box::pin(async move { Err(ContextPreparationFailure::Continuity(cause)) })
    }

    fn plan(
        &mut self,
        _step: StepNumber,
        _ordinal: u32,
        _generation: HistoryGeneration,
    ) -> Result<PlannedAttempt, PlanRefusal> {
        panic!("a refused continuity refresh must not reach request planning")
    }
}

impl AttemptPlan for StructuralRefusalPlan {
    fn plan(
        &mut self,
        _step: StepNumber,
        _ordinal: u32,
        _generation: HistoryGeneration,
    ) -> Result<PlannedAttempt, PlanRefusal> {
        Err(PlanRefusal::new(
            "bounded durable history requires compaction".to_owned(),
            Some(grapt_types::CompactionTrigger::StructuralBound),
        ))
    }
}

impl AttemptPlan for StalePlan {
    fn plan(
        &mut self,
        _step: StepNumber,
        _ordinal: u32,
        _generation: HistoryGeneration,
    ) -> Result<PlannedAttempt, PlanRefusal> {
        let Some(authority) = self.authority.take() else {
            return Err(PlanRefusal::new(
                "the stale authority was already consumed".to_owned(),
                None,
            ));
        };
        let request = request_for(&authority);
        Ok(PlannedAttempt {
            operation_mail: None,
            mail: None,
            context: None,
            authority,
            request: ProviderRequest::ChatGptCodex(request),
        })
    }
}

impl AttemptPlan for Box<dyn AttemptPlan + '_> {
    fn context_preparation_progress(
        &self,
    ) -> Option<tokio::sync::watch::Receiver<crate::ContextPreparationStage>> {
        self.as_ref().context_preparation_progress()
    }

    fn retrying(&mut self, notice: Option<crate::step::RetryNotice>) {
        self.as_mut().retrying(notice);
    }

    fn admitted(&mut self, attempt: &PhysicalAttemptId) {
        self.as_mut().admitted(attempt);
    }

    fn context_admission(
        &mut self,
        planned: &PlannedAttempt,
    ) -> Result<Option<grapt_types::CompactionTrigger>, ContextAdmissionRefusal> {
        self.as_mut().context_admission(planned)
    }

    fn refresh_context(
        &mut self,
        step: StepNumber,
        generation: HistoryGeneration,
    ) -> crate::BoxFuture<'_, Result<(), ContextPreparationFailure>> {
        self.as_mut().refresh_context(step, generation)
    }

    fn shutdown(&mut self) -> crate::BoxFuture<'_, Result<(), String>> {
        self.as_mut().shutdown()
    }

    fn finish_context(&mut self, _cancelled: bool) -> crate::BoxFuture<'_, Result<(), String>> {
        self.as_mut().finish_context(_cancelled)
    }

    fn plan(
        &mut self,
        step: StepNumber,
        ordinal: u32,
        generation: HistoryGeneration,
    ) -> Result<PlannedAttempt, PlanRefusal> {
        self.as_mut().plan(step, ordinal, generation)
    }
}

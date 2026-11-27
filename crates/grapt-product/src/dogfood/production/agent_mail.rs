use super::*;

impl ProductPlan {
    pub(super) fn operation_mail(
        &self,
        step: StepNumber,
        input: &mut Vec<grapt_provider::CodexInputItem>,
    ) -> Result<Option<grapt_inbox::operation_mail::OperationMailBatch>, PlanRefusal> {
        let batch = match &self.context {
            PlanContext::Durable(continuation) => continuation.operation_mail(step),
            PlanContext::Transient(_) => Ok(None),
        }
        .map_err(|detail| PlanRefusal::new(detail, None))?;
        if let Some(batch) = &batch {
            input.extend(batch.contents().iter().map(|content| {
                grapt_provider::CodexInputItem::Message(grapt_provider::InputMessage {
                    role: grapt_provider::MessageRole::User,
                    content: content.clone(),
                })
            }));
        }
        Ok(batch)
    }
}
